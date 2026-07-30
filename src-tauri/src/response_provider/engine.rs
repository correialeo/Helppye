//! Orquestra a geração de sugestão de resposta: decide quando gerar (turno elegível com
//! utterance recém-finalizada), monta o contexto, chama o `ResponseProvider` ativo e
//! transmite os deltas como eventos. Cancela/substitui uma geração em andamento quando a
//! mesma pessoa continua falando no mesmo turno, para nunca responder por cima de uma
//! fala que ainda não terminou.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use tauri::AppHandle;
use tokio_util::sync::CancellationToken;

use crate::audio::types::AudioSource;
use crate::conversation::{
    ConversationSpeaker, ConversationTimelineEvent, ConversationTurn, TurnId,
};

/// Contexto de disparo capturado no momento em que `process_conversation_events` recebe
/// um `UtteranceFinalized` elegível. `utterance_finalized_at` é um `Instant` (não epoch)
/// capturado no início do processamento desse evento — como a finalização (reativa ou
/// pelo timer dedicado) e este processamento acontecem na mesma pilha de chamada
/// síncrona, isso equivale, na prática, ao instante real de finalização, sem precisar
/// serializar `Instant` através do evento da timeline.
pub struct GenerationTrigger {
    pub utterance_finalized_at: Instant,
    pub finalization_reason: String,
    pub gap_ms_used: u64,
    pub silence_detected_ms: Option<u64>,
}

use super::anthropic::AnthropicProvider;
use super::config_store::{self, ResponseProviderConfig, ResponseProviderKind};
use super::context::build_request;
use super::events::{
    emit_response_suggestion_event, GenerationDiagnostics, ResponseSuggestionEvent,
};
use super::ollama::OllamaProvider;
use super::openai_compatible::OpenAiCompatibleProvider;
use super::provider::{ResponseChunk, ResponseProvider, ResponseProviderError, ResponseRequest};
use super::secrets;
use super::skip_detector::{SkipDecision, SkipDetector};

const MAX_HISTORY_TURNS: usize = 20;

/// Motivo de cancelamento hoje é sempre este: uma nova utterance no mesmo turno
/// substituiu a geração em andamento (ver `trigger_generation`).
const CANCEL_REASON_NEW_UTTERANCE: &str = "new_utterance";

/// Quantos caracteres brutos (antes do `SkipDetector`) manter para diagnóstico. Cobre
/// folgadamente o marcador `[SKIP]`, sem acumular texto de respostas longas à toa.
const RAW_PREFIX_CAP_CHARS: usize = 80;

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

struct GenerationHandle {
    generation_id: u64,
    cancel: CancellationToken,
}

struct MisconfiguredProvider {
    message: String,
}

#[async_trait::async_trait]
impl ResponseProvider for MisconfiguredProvider {
    fn provider_name(&self) -> &'static str {
        "misconfigured"
    }

    async fn stream_reply(
        &self,
        _request: ResponseRequest,
    ) -> Result<
        (
            super::provider::ResponseStream,
            super::provider::ResponseStreamMeta,
        ),
        ResponseProviderError,
    > {
        Err(ResponseProviderError::Credential(self.message.clone()))
    }
}

fn build_provider(config: &ResponseProviderConfig) -> Arc<dyn ResponseProvider> {
    match config.provider {
        ResponseProviderKind::Ollama => Arc::new(OllamaProvider::new(
            config.base_url.clone(),
            config.model.clone(),
            config.ollama_keep_alive.clone(),
        )),
        ResponseProviderKind::OpenAi => match secrets::load_api_key(ResponseProviderKind::OpenAi) {
            Ok(Some(api_key)) => Arc::new(OpenAiCompatibleProvider::openai(
                api_key,
                config.model.clone(),
                config.base_url.clone(),
            )),
            Ok(None) => misconfigured("nenhuma API key da OpenAI configurada"),
            Err(e) => misconfigured(&format!("falha ao ler a API key da OpenAI: {e}")),
        },
        ResponseProviderKind::DeepSeek => {
            match secrets::load_api_key(ResponseProviderKind::DeepSeek) {
                Ok(Some(api_key)) => Arc::new(OpenAiCompatibleProvider::deepseek(
                    api_key,
                    config.model.clone(),
                    config.base_url.clone(),
                )),
                Ok(None) => misconfigured("nenhuma API key da DeepSeek configurada"),
                Err(e) => misconfigured(&format!("falha ao ler a API key da DeepSeek: {e}")),
            }
        }
        ResponseProviderKind::Anthropic => {
            match secrets::load_api_key(ResponseProviderKind::Anthropic) {
                Ok(Some(api_key)) => Arc::new(AnthropicProvider::new(
                    api_key,
                    config.model.clone(),
                    config.base_url.clone(),
                )),
                Ok(None) => misconfigured("nenhuma API key da Anthropic configurada"),
                Err(e) => misconfigured(&format!("falha ao ler a API key da Anthropic: {e}")),
            }
        }
    }
}

fn misconfigured(message: &str) -> Arc<dyn ResponseProvider> {
    Arc::new(MisconfiguredProvider {
        message: message.to_string(),
    })
}

pub fn is_eligible_turn(turn: &ConversationTurn) -> bool {
    turn.speaker == ConversationSpeaker::OtherPerson && turn.source == AudioSource::SystemOutput
}

pub struct ResponseEngine {
    provider: Mutex<Arc<dyn ResponseProvider>>,
    config: Mutex<ResponseProviderConfig>,
    config_path: PathBuf,
    history: Mutex<VecDeque<ConversationTurn>>,
    generations: Mutex<HashMap<TurnId, GenerationHandle>>,
    next_generation_id: AtomicU64,
}

impl ResponseEngine {
    pub fn from_config_path(config_path: PathBuf) -> Self {
        let config = config_store::load(&config_path);
        let provider = build_provider(&config);
        ResponseEngine {
            provider: Mutex::new(provider),
            config: Mutex::new(config),
            config_path,
            history: Mutex::new(VecDeque::with_capacity(MAX_HISTORY_TURNS)),
            generations: Mutex::new(HashMap::new()),
            next_generation_id: AtomicU64::new(0),
        }
    }

    pub fn current_config(&self) -> ResponseProviderConfig {
        self.config
            .lock()
            .expect("response engine mutex poisoned")
            .clone()
    }

    pub fn update_config(&self, config: ResponseProviderConfig) -> Result<(), String> {
        config_store::save(&self.config_path, &config)?;
        let provider = build_provider(&config);
        *self
            .provider
            .lock()
            .expect("response engine mutex poisoned") = provider;
        *self.config.lock().expect("response engine mutex poisoned") = config;
        Ok(())
    }

    /// Reconstrói o provedor ativo se ele for do tipo cuja credencial acabou de mudar
    /// (salva/removida) — evita exigir que o usuário reenvie a configuração inteira só
    /// para uma troca de API key.
    pub fn reload_provider_if_current(&self, changed: ResponseProviderKind) {
        let config = self.current_config();
        if config.provider == changed {
            let provider = build_provider(&config);
            *self
                .provider
                .lock()
                .expect("response engine mutex poisoned") = provider;
        }
    }

    fn push_history(&self, turn: ConversationTurn) {
        let mut history = self.history.lock().expect("response engine mutex poisoned");
        history.retain(|existing| existing.id != turn.id);
        history.push_back(turn);
        while history.len() > MAX_HISTORY_TURNS {
            history.pop_front();
        }
    }

    fn history_snapshot(&self) -> Vec<ConversationTurn> {
        self.history
            .lock()
            .expect("response engine mutex poisoned")
            .iter()
            .cloned()
            .collect()
    }

    fn clear_if_current(&self, turn_id: TurnId, generation_id: u64) {
        let mut generations = self
            .generations
            .lock()
            .expect("response engine mutex poisoned");
        if generations.get(&turn_id).map(|h| h.generation_id) == Some(generation_id) {
            generations.remove(&turn_id);
        }
    }

    /// Genérica sobre `R: tauri::Runtime` (em vez do `AppHandle` = `AppHandle<Wry>` fixo
    /// usado em produção) só para poder ser exercitada em teste com
    /// `tauri::test::mock_app`, que produz um `AppHandle<MockRuntime>` — não precisa de
    /// janela/webview real para verificar disparo, cancelamento e liberação de estado.
    pub fn trigger_generation<R: tauri::Runtime>(
        self: Arc<Self>,
        app: AppHandle<R>,
        turn: ConversationTurn,
        trigger: GenerationTrigger,
    ) {
        let generation_id = self.next_generation_id.fetch_add(1, Ordering::Relaxed) + 1;
        let cancel_token = CancellationToken::new();

        {
            let mut generations = self
                .generations
                .lock()
                .expect("response engine mutex poisoned");
            if let Some(previous) = generations.insert(
                turn.id,
                GenerationHandle {
                    generation_id,
                    cancel: cancel_token.clone(),
                },
            ) {
                previous.cancel.cancel();
                emit_response_suggestion_event(
                    &app,
                    ResponseSuggestionEvent::Cancelled {
                        turn_id: turn.id,
                        generation_id: previous.generation_id,
                    },
                );
            }
        }

        let engine = self.clone();
        tauri::async_runtime::spawn(async move {
            engine
                .run_generation(app, turn, generation_id, cancel_token, trigger)
                .await;
        });
    }

    async fn run_generation<R: tauri::Runtime>(
        self: Arc<Self>,
        app: AppHandle<R>,
        turn: ConversationTurn,
        generation_id: u64,
        cancel_token: CancellationToken,
        trigger: GenerationTrigger,
    ) {
        let provider = self
            .provider
            .lock()
            .expect("response engine mutex poisoned")
            .clone();
        let model = self.current_config().model;
        let history = self.history_snapshot();
        let request = build_request(&history, &turn);

        tracing::info!(
            provider = provider.provider_name(),
            turn_id = turn.id.value(),
            generation_id,
            "starting response generation"
        );

        emit_response_suggestion_event(
            &app,
            ResponseSuggestionEvent::Started {
                turn_id: turn.id,
                generation_id,
            },
        );

        let started_at = Instant::now();
        let mut diagnostics = GenerationDiagnostics {
            generation_id,
            turn_id: turn.id,
            provider: provider.provider_name().to_string(),
            model,
            request_started: epoch_ms(),
            http_status: None,
            first_chunk_received: None,
            raw_prefix: String::new(),
            skip_detected: false,
            cancel_reason: None,
            latency_ms: 0,
            final_text_length: 0,
            event_emitted: String::new(),
            finalization_reason: trigger.finalization_reason.clone(),
            gap_ms_used: trigger.gap_ms_used,
            silence_detected_ms: trigger.silence_detected_ms,
            utterance_finalized_to_request_started_ms: None,
            request_to_first_http_chunk_ms: None,
            request_to_first_visible_token_ms: None,
            end_of_speech_to_first_visible_token_ms: None,
        };
        let utterance_finalized_at = trigger.utterance_finalized_at;
        // Marcos de latência com relógio monotônico — computados em `finish_generation`,
        // chamada em todo caminho de saída, para não repetir a conta em cada `return`.
        let request_started_at = Instant::now();
        let mut first_http_chunk_at: Option<Instant> = None;
        let mut first_visible_text_at: Option<Instant> = None;

        macro_rules! finish {
            ($diagnostics:expr) => {
                self.finish_generation(
                    &app,
                    &turn,
                    generation_id,
                    $diagnostics,
                    started_at,
                    utterance_finalized_at,
                    request_started_at,
                    first_http_chunk_at,
                    first_visible_text_at,
                )
            };
        }

        let stream_result = tokio::select! {
            _ = cancel_token.cancelled() => {
                diagnostics.cancel_reason = Some(CANCEL_REASON_NEW_UTTERANCE.to_string());
                diagnostics.event_emitted = "cancelled".to_string();
                finish!(diagnostics);
                return;
            }
            result = provider.stream_reply(request) => result,
        };

        let mut stream = match stream_result {
            Ok((s, meta)) => {
                diagnostics.http_status = Some(meta.http_status);
                s
            }
            Err(e) => {
                diagnostics.event_emitted = "error".to_string();
                emit_response_suggestion_event(
                    &app,
                    ResponseSuggestionEvent::Error {
                        turn_id: turn.id,
                        generation_id,
                        message: e.to_string(),
                    },
                );
                finish!(diagnostics);
                return;
            }
        };

        let mut detector = SkipDetector::new();
        let mut full_text = String::new();

        loop {
            let next = tokio::select! {
                _ = cancel_token.cancelled() => {
                    diagnostics.cancel_reason = Some(CANCEL_REASON_NEW_UTTERANCE.to_string());
                    diagnostics.event_emitted = "cancelled".to_string();
                    emit_response_suggestion_event(
                        &app,
                        ResponseSuggestionEvent::Cancelled { turn_id: turn.id, generation_id },
                    );
                    finish!(diagnostics);
                    return;
                }
                item = stream.next() => item,
            };

            let Some(item) = next else {
                break;
            };

            if diagnostics.first_chunk_received.is_none() {
                diagnostics.first_chunk_received = Some(epoch_ms());
            }
            if first_http_chunk_at.is_none() {
                first_http_chunk_at = Some(Instant::now());
            }

            match item {
                Ok(ResponseChunk::Delta(text)) => {
                    if diagnostics.raw_prefix.chars().count() < RAW_PREFIX_CAP_CHARS {
                        diagnostics.raw_prefix.push_str(&text);
                    }
                    match detector.push(&text) {
                        SkipDecision::Pending => {}
                        SkipDecision::Skip => {
                            diagnostics.skip_detected = true;
                            diagnostics.event_emitted = "skipped".to_string();
                            emit_response_suggestion_event(
                                &app,
                                ResponseSuggestionEvent::Skipped {
                                    turn_id: turn.id,
                                    generation_id,
                                },
                            );
                            finish!(diagnostics);
                            return;
                        }
                        SkipDecision::NotSkip { flush } => {
                            if !flush.is_empty() {
                                if first_visible_text_at.is_none() {
                                    first_visible_text_at = Some(Instant::now());
                                }
                                full_text.push_str(&flush);
                                emit_response_suggestion_event(
                                    &app,
                                    ResponseSuggestionEvent::Delta {
                                        turn_id: turn.id,
                                        generation_id,
                                        text: flush,
                                    },
                                );
                            }
                        }
                    }
                }
                Ok(ResponseChunk::Done) => break,
                Err(e) => {
                    diagnostics.event_emitted = "error".to_string();
                    emit_response_suggestion_event(
                        &app,
                        ResponseSuggestionEvent::Error {
                            turn_id: turn.id,
                            generation_id,
                            message: e.to_string(),
                        },
                    );
                    finish!(diagnostics);
                    return;
                }
            }
        }

        match detector.finish() {
            SkipDecision::Skip => {
                diagnostics.skip_detected = true;
                diagnostics.event_emitted = "skipped".to_string();
                emit_response_suggestion_event(
                    &app,
                    ResponseSuggestionEvent::Skipped {
                        turn_id: turn.id,
                        generation_id,
                    },
                );
            }
            SkipDecision::NotSkip { flush } => {
                if !flush.is_empty() {
                    if first_visible_text_at.is_none() {
                        first_visible_text_at = Some(Instant::now());
                    }
                    full_text.push_str(&flush);
                    emit_response_suggestion_event(
                        &app,
                        ResponseSuggestionEvent::Delta {
                            turn_id: turn.id,
                            generation_id,
                            text: flush,
                        },
                    );
                }
                diagnostics.event_emitted = if full_text.trim().is_empty() {
                    "completed_empty".to_string()
                } else {
                    "completed_with_text".to_string()
                };
                emit_response_suggestion_event(
                    &app,
                    ResponseSuggestionEvent::Completed {
                        turn_id: turn.id,
                        generation_id,
                        text: full_text.clone(),
                    },
                );
            }
            SkipDecision::Pending => {
                diagnostics.event_emitted = if full_text.trim().is_empty() {
                    "completed_empty".to_string()
                } else {
                    "completed_with_text".to_string()
                };
                emit_response_suggestion_event(
                    &app,
                    ResponseSuggestionEvent::Completed {
                        turn_id: turn.id,
                        generation_id,
                        text: full_text.clone(),
                    },
                );
            }
        }

        diagnostics.final_text_length = full_text.chars().count();
        finish!(diagnostics);
    }

    /// Fecha uma geração: registra `latency_ms`, computa as métricas de latência de ponta
    /// a ponta (relógio monotônico, não epoch), emite o evento de diagnóstico e libera o
    /// slot de `generations` se ainda for a geração corrente para o turno. Chamada em todo
    /// caminho de saída de `run_generation` (skip, erro, cancelamento ou conclusão).
    #[allow(clippy::too_many_arguments)]
    fn finish_generation<R: tauri::Runtime>(
        &self,
        app: &AppHandle<R>,
        turn: &ConversationTurn,
        generation_id: u64,
        mut diagnostics: GenerationDiagnostics,
        started_at: Instant,
        utterance_finalized_at: Instant,
        request_started_at: Instant,
        first_http_chunk_at: Option<Instant>,
        first_visible_text_at: Option<Instant>,
    ) {
        diagnostics.latency_ms = started_at.elapsed().as_millis() as u64;
        diagnostics.utterance_finalized_to_request_started_ms = Some(
            request_started_at
                .saturating_duration_since(utterance_finalized_at)
                .as_millis() as u64,
        );
        diagnostics.request_to_first_http_chunk_ms = first_http_chunk_at
            .map(|t| t.saturating_duration_since(request_started_at).as_millis() as u64);
        diagnostics.request_to_first_visible_token_ms = first_visible_text_at
            .map(|t| t.saturating_duration_since(request_started_at).as_millis() as u64);
        diagnostics.end_of_speech_to_first_visible_token_ms = first_visible_text_at.map(|t| {
            t.saturating_duration_since(utterance_finalized_at)
                .as_millis() as u64
        });
        tracing::debug!(?diagnostics, "response generation diagnostics");
        emit_response_suggestion_event(
            app,
            ResponseSuggestionEvent::Diagnostics(Box::new(diagnostics)),
        );
        self.clear_if_current(turn.id, generation_id);
    }
}

/// Chamado a cada lote de eventos da Conversation Timeline. Mantém o histórico rolante
/// e dispara geração quando uma utterance de um turno elegível finaliza. Genérica sobre
/// `R: tauri::Runtime` pelo mesmo motivo de `trigger_generation`.
pub fn process_conversation_events<R: tauri::Runtime>(
    app: &AppHandle<R>,
    engine: Arc<ResponseEngine>,
    events: &[ConversationTimelineEvent],
) {
    let mut latest_turns: HashMap<TurnId, ConversationTurn> = HashMap::new();
    for event in events {
        match event {
            ConversationTimelineEvent::TurnUpdated { turn }
            | ConversationTimelineEvent::TurnFinalized { turn } => {
                latest_turns.insert(turn.id, turn.clone());
            }
            _ => {}
        }
    }

    // Capturado uma única vez, antes do loop: a finalização da utterance (reativa ou pelo
    // timer dedicado) e este processamento acontecem na mesma pilha de chamada síncrona,
    // então este `Instant` é, na prática, o instante de finalização — ver
    // `GenerationTrigger`.
    let utterance_finalized_at = Instant::now();

    for event in events {
        match event {
            ConversationTimelineEvent::TurnFinalized { turn } => {
                engine.push_history(turn.clone());
            }
            ConversationTimelineEvent::UtteranceFinalized {
                turn_id,
                finalization_reason,
                gap_ms_used,
                silence_detected_ms,
                ..
            } => {
                if let Some(turn) = latest_turns.get(turn_id) {
                    if is_eligible_turn(turn) {
                        let trigger = GenerationTrigger {
                            utterance_finalized_at,
                            finalization_reason: finalization_reason.as_str().to_string(),
                            gap_ms_used: *gap_ms_used,
                            silence_detected_ms: *silence_detected_ms,
                        };
                        engine
                            .clone()
                            .trigger_generation(app.clone(), turn.clone(), trigger);
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::segment::{AudioTimestamp, SegmentId};
    use crate::conversation::{
        ConversationUtterance, SessionId, UtteranceFinalizationReason, UtteranceId,
    };
    use crate::response_provider::provider::{ResponseStream, ResponseStreamMeta};
    use futures_util::stream;
    use std::time::Duration;
    use tauri::Listener;
    use tokio::sync::mpsc;

    // `ResponseEngine::from_config_path` reads/derives config from disk and picks a real
    // provider by kind — not useful for testing the orchestration logic in this file.
    // This constructor plugs an arbitrary fake `ResponseProvider` directly, bypassing
    // config I/O entirely.
    impl ResponseEngine {
        fn for_test(provider: Arc<dyn ResponseProvider>) -> Arc<Self> {
            Arc::new(ResponseEngine {
                provider: Mutex::new(provider),
                config: Mutex::new(ResponseProviderConfig::default()),
                config_path: PathBuf::from("unused-in-tests.json"),
                history: Mutex::new(VecDeque::new()),
                generations: Mutex::new(HashMap::new()),
                next_generation_id: AtomicU64::new(0),
            })
        }
    }

    // Builds a fresh stream on every `stream_reply` call (not a one-shot `take()`) so the
    // same provider instance can be reused across multiple generations within one test —
    // exactly like the real, config-driven providers, which are only rebuilt when the
    // response provider configuration changes, not once per generation.
    enum FakeBehavior {
        RepliesWith(String),
        /// Never yields anything and never ends — stays "in flight" until the caller
        /// cancels it (via a new trigger for the same turn) or the test drops it. Used to
        /// test cancellation/replacement of an active generation.
        Hangs,
    }

    struct FakeProvider {
        behavior: FakeBehavior,
    }

    impl FakeProvider {
        fn with_text(text: &str) -> Arc<Self> {
            Arc::new(FakeProvider {
                behavior: FakeBehavior::RepliesWith(text.to_string()),
            })
        }

        fn hanging() -> Arc<Self> {
            Arc::new(FakeProvider {
                behavior: FakeBehavior::Hangs,
            })
        }
    }

    #[async_trait::async_trait]
    impl ResponseProvider for FakeProvider {
        fn provider_name(&self) -> &'static str {
            "fake"
        }

        async fn stream_reply(
            &self,
            _request: ResponseRequest,
        ) -> Result<(ResponseStream, ResponseStreamMeta), ResponseProviderError> {
            let stream: ResponseStream = match &self.behavior {
                FakeBehavior::RepliesWith(text) => {
                    Box::pin(stream::iter(vec![Ok(ResponseChunk::Delta(text.clone()))]))
                }
                FakeBehavior::Hangs => Box::pin(stream::pending()),
            };
            Ok((stream, ResponseStreamMeta { http_status: 200 }))
        }
    }

    fn turn(id: u64, speaker: ConversationSpeaker, source: AudioSource) -> ConversationTurn {
        ConversationTurn {
            id: TurnId::from_raw(id),
            speaker,
            source,
            text: "texto do turno".to_string(),
            utterances: Vec::new(),
            started_at: AudioTimestamp(0),
            ended_at: AudioTimestamp(1_000),
            finalized_at: None,
        }
    }

    fn utterance_finalized_batch(turn: &ConversationTurn) -> Vec<ConversationTimelineEvent> {
        let utterance = ConversationUtterance {
            id: UtteranceId::from_raw(1),
            speaker: turn.speaker,
            source: turn.source,
            text: turn.text.clone(),
            segments: vec![SegmentId::next()],
            started_at: turn.started_at,
            ended_at: turn.ended_at,
            finalized_at: Some(turn.ended_at),
            revision: 1,
        };
        vec![
            ConversationTimelineEvent::UtteranceFinalized {
                turn_id: turn.id,
                utterance,
                finalization_reason: UtteranceFinalizationReason::InactivityTimeout,
                gap_ms_used: 1_800,
                silence_detected_ms: Some(1_800),
                session_id: SessionId::new(),
            },
            ConversationTimelineEvent::TurnUpdated { turn: turn.clone() },
        ]
    }

    fn capture_events(
        app: &AppHandle<tauri::test::MockRuntime>,
    ) -> mpsc::UnboundedReceiver<serde_json::Value> {
        let (tx, rx) = mpsc::unbounded_channel();
        app.listen_any(
            super::super::events::RESPONSE_SUGGESTION_EVENT,
            move |event| {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(event.payload()) {
                    let _ = tx.send(value);
                }
            },
        );
        rx
    }

    /// Waits for the next captured event whose `type` matches `event_type`, ignoring any
    /// others in between (e.g. `delta` events before a `completed`). Bounded by a real
    /// timeout only as a safety net against a genuinely hung test, not to simulate
    /// business-logic timing.
    async fn wait_for_event_type(
        rx: &mut mpsc::UnboundedReceiver<serde_json::Value>,
        event_type: &str,
    ) -> serde_json::Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let value = rx.recv().await.expect("event channel closed unexpectedly");
                if value["type"] == event_type {
                    return value;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for a {event_type:?} event"))
    }

    /// Like `wait_for_event_type`, but returns on the first event matching any of
    /// `event_types` — used when the test needs to observe whether an event of one type
    /// (e.g. `cancelled`) happens before another (e.g. `diagnostics`) without racing a
    /// real-time timeout against it.
    async fn wait_for_event_type_any(
        rx: &mut mpsc::UnboundedReceiver<serde_json::Value>,
        event_types: &[&str],
    ) -> serde_json::Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let value = rx.recv().await.expect("event channel closed unexpectedly");
                if event_types.iter().any(|t| value["type"] == *t) {
                    return value;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for one of {event_types:?}"))
    }

    #[test]
    fn only_other_person_system_output_turns_are_eligible() {
        let remote = turn(
            1,
            ConversationSpeaker::OtherPerson,
            AudioSource::SystemOutput,
        );
        let user = turn(2, ConversationSpeaker::User, AudioSource::Microphone);
        assert!(is_eligible_turn(&remote));
        assert!(!is_eligible_turn(&user));
    }

    #[tokio::test]
    async fn eligible_utterance_triggers_generation_and_user_speech_does_not() {
        let engine = ResponseEngine::for_test(FakeProvider::with_text("resposta sugerida"));
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let mut rx = capture_events(&handle);

        let remote_turn = turn(
            1,
            ConversationSpeaker::OtherPerson,
            AudioSource::SystemOutput,
        );
        let user_turn = turn(2, ConversationSpeaker::User, AudioSource::Microphone);

        // Fala do usuário nunca deveria disparar uma sugestão de resposta — processada
        // primeiro para garantir que, se disparasse por engano, o evento chegaria antes do
        // da fala elegível.
        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch(&user_turn),
        );
        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch(&remote_turn),
        );

        let started = wait_for_event_type(&mut rx, "started").await;
        assert_eq!(
            started["turn_id"], 1,
            "only the eligible (remote) turn generated a suggestion"
        );
        wait_for_event_type(&mut rx, "completed").await;
    }

    #[tokio::test]
    async fn a_new_trigger_for_the_same_turn_cancels_the_generation_still_in_flight() {
        let engine = ResponseEngine::for_test(FakeProvider::hanging());
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let mut rx = capture_events(&handle);

        let remote_turn = turn(
            1,
            ConversationSpeaker::OtherPerson,
            AudioSource::SystemOutput,
        );
        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch(&remote_turn),
        );
        let first_started = wait_for_event_type(&mut rx, "started").await;
        let first_generation_id = first_started["generation_id"].clone();

        // A pessoa continua falando no mesmo turno: uma segunda utterance finaliza para o
        // mesmo turn_id enquanto a primeira geração (que nunca produz nada, de propósito)
        // ainda está em andamento.
        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch(&remote_turn),
        );

        let cancelled = wait_for_event_type(&mut rx, "cancelled").await;
        assert_eq!(
            cancelled["generation_id"], first_generation_id,
            "the generation that got cancelled is the one still in flight, not the new one"
        );
        let second_started = wait_for_event_type(&mut rx, "started").await;
        assert_ne!(
            second_started["generation_id"], first_generation_id,
            "the replacement generation has its own, newer id"
        );
    }

    #[tokio::test]
    async fn generation_state_is_released_after_completion_no_leftover_cancellation_later() {
        let engine = ResponseEngine::for_test(FakeProvider::with_text("ok"));
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let mut rx = capture_events(&handle);

        let remote_turn = turn(
            1,
            ConversationSpeaker::OtherPerson,
            AudioSource::SystemOutput,
        );
        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch(&remote_turn),
        );
        wait_for_event_type(&mut rx, "diagnostics").await; // last event of the first generation

        // A primeira geração já terminou naturalmente (não foi cancelada) — se o estado em
        // `generations` não tivesse sido liberado em `finish_generation`, este novo
        // disparo para o mesmo turno enxergaria uma entrada "fantasma" e emitiria um
        // `cancelled` espúrio antes do novo `started`.
        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch(&remote_turn),
        );
        let mut saw_cancelled = false;
        loop {
            let value = wait_for_event_type_any(&mut rx, &["cancelled", "diagnostics"]).await;
            if value["type"] == "cancelled" {
                saw_cancelled = true;
            }
            if value["type"] == "diagnostics" {
                break; // last event of the second generation
            }
        }
        assert!(
            !saw_cancelled,
            "no generation was still active for this turn, so nothing should have been cancelled"
        );
    }

    #[tokio::test]
    async fn end_of_speech_to_first_visible_token_ms_is_computed_from_the_trigger() {
        let engine = ResponseEngine::for_test(FakeProvider::with_text("resposta útil"));
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let mut rx = capture_events(&handle);

        let remote_turn = turn(
            1,
            ConversationSpeaker::OtherPerson,
            AudioSource::SystemOutput,
        );
        let batch = utterance_finalized_batch(&remote_turn);
        process_conversation_events(&handle, engine.clone(), &batch);

        let diagnostics = wait_for_event_type(&mut rx, "diagnostics").await;
        assert_eq!(diagnostics["finalization_reason"], "inactivity_timeout");
        assert_eq!(diagnostics["gap_ms_used"], 1_800);
        assert_eq!(diagnostics["silence_detected_ms"], 1_800);
        assert!(
            diagnostics["end_of_speech_to_first_visible_token_ms"].is_u64(),
            "end_of_speech_to_first_visible_token_ms should be a measured value, got {:?}",
            diagnostics["end_of_speech_to_first_visible_token_ms"]
        );
        assert!(diagnostics["request_to_first_visible_token_ms"].is_u64());
        assert!(diagnostics["utterance_finalized_to_request_started_ms"].is_u64());
        assert_eq!(diagnostics["event_emitted"], "completed_with_text");
    }
}
