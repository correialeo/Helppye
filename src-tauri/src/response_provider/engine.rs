//! Orquestra a geração de sugestão de resposta: decide quando gerar (turno elegível com
//! utterance recém-finalizada), monta o contexto, chama o `ResponseProvider` ativo e
//! transmite os deltas como eventos. Cancela/substitui uma geração em andamento quando a
//! mesma pessoa continua falando no mesmo turno, para nunca responder por cima de uma
//! fala que ainda não terminou.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use tauri::AppHandle;
use tokio_util::sync::CancellationToken;

use crate::audio::types::AudioSource;
use crate::conversation::{
    ConversationSpeaker, ConversationTimelineEvent, ConversationTurn, TurnId,
};

use super::anthropic::AnthropicProvider;
use super::config_store::{self, ResponseProviderConfig, ResponseProviderKind};
use super::context::build_request;
use super::events::{emit_response_suggestion_event, ResponseSuggestionEvent};
use super::ollama::OllamaProvider;
use super::openai_compatible::OpenAiCompatibleProvider;
use super::provider::{ResponseChunk, ResponseProvider, ResponseProviderError, ResponseRequest};
use super::secrets;
use super::skip_detector::{SkipDecision, SkipDetector};

const MAX_HISTORY_TURNS: usize = 20;

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
    ) -> Result<super::provider::ResponseStream, ResponseProviderError> {
        Err(ResponseProviderError::Credential(self.message.clone()))
    }
}

fn build_provider(config: &ResponseProviderConfig) -> Arc<dyn ResponseProvider> {
    match config.provider {
        ResponseProviderKind::Ollama => Arc::new(OllamaProvider::new(
            config.base_url.clone(),
            config.model.clone(),
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

    pub fn trigger_generation(self: Arc<Self>, app: AppHandle, turn: ConversationTurn) {
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
                .run_generation(app, turn, generation_id, cancel_token)
                .await;
        });
    }

    async fn run_generation(
        self: Arc<Self>,
        app: AppHandle,
        turn: ConversationTurn,
        generation_id: u64,
        cancel_token: CancellationToken,
    ) {
        let provider = self
            .provider
            .lock()
            .expect("response engine mutex poisoned")
            .clone();
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

        let stream_result = tokio::select! {
            _ = cancel_token.cancelled() => {
                self.clear_if_current(turn.id, generation_id);
                return;
            }
            result = provider.stream_reply(request) => result,
        };

        let mut stream = match stream_result {
            Ok(s) => s,
            Err(e) => {
                emit_response_suggestion_event(
                    &app,
                    ResponseSuggestionEvent::Error {
                        turn_id: turn.id,
                        generation_id,
                        message: e.to_string(),
                    },
                );
                self.clear_if_current(turn.id, generation_id);
                return;
            }
        };

        let mut detector = SkipDetector::new();
        let mut full_text = String::new();

        loop {
            let next = tokio::select! {
                _ = cancel_token.cancelled() => {
                    emit_response_suggestion_event(
                        &app,
                        ResponseSuggestionEvent::Cancelled { turn_id: turn.id, generation_id },
                    );
                    self.clear_if_current(turn.id, generation_id);
                    return;
                }
                item = stream.next() => item,
            };

            let Some(item) = next else {
                break;
            };

            match item {
                Ok(ResponseChunk::Delta(text)) => match detector.push(&text) {
                    SkipDecision::Pending => {}
                    SkipDecision::Skip => {
                        emit_response_suggestion_event(
                            &app,
                            ResponseSuggestionEvent::Skipped {
                                turn_id: turn.id,
                                generation_id,
                            },
                        );
                        self.clear_if_current(turn.id, generation_id);
                        return;
                    }
                    SkipDecision::NotSkip { flush } => {
                        if !flush.is_empty() {
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
                },
                Ok(ResponseChunk::Done) => break,
                Err(e) => {
                    emit_response_suggestion_event(
                        &app,
                        ResponseSuggestionEvent::Error {
                            turn_id: turn.id,
                            generation_id,
                            message: e.to_string(),
                        },
                    );
                    self.clear_if_current(turn.id, generation_id);
                    return;
                }
            }
        }

        match detector.finish() {
            SkipDecision::Skip => {
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
                emit_response_suggestion_event(
                    &app,
                    ResponseSuggestionEvent::Completed {
                        turn_id: turn.id,
                        generation_id,
                        text: full_text,
                    },
                );
            }
            SkipDecision::Pending => {
                emit_response_suggestion_event(
                    &app,
                    ResponseSuggestionEvent::Completed {
                        turn_id: turn.id,
                        generation_id,
                        text: full_text,
                    },
                );
            }
        }

        self.clear_if_current(turn.id, generation_id);
    }
}

/// Chamado a cada lote de eventos da Conversation Timeline. Mantém o histórico rolante
/// e dispara geração quando uma utterance de um turno elegível finaliza.
pub fn process_conversation_events(
    app: &AppHandle,
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

    for event in events {
        match event {
            ConversationTimelineEvent::TurnFinalized { turn } => {
                engine.push_history(turn.clone());
            }
            ConversationTimelineEvent::UtteranceFinalized { turn_id, .. } => {
                if let Some(turn) = latest_turns.get(turn_id) {
                    if is_eligible_turn(turn) {
                        engine.clone().trigger_generation(app.clone(), turn.clone());
                    }
                }
            }
            _ => {}
        }
    }
}
