//! Orquestra a geração de sugestão de resposta: decide quando gerar (turno elegível com
//! utterance recém-finalizada), monta o contexto, chama o `ResponseProvider` ativo e
//! transmite os deltas como eventos. Cancela/substitui uma geração em andamento quando a
//! mesma pessoa continua falando no mesmo turno, para nunca responder por cima de uma
//! fala que ainda não terminou.
//!
//! **A sessão é uma fronteira rígida.** Todo estado conversacional (histórico usado no
//! prompt, gerações ativas, token de cancelamento) pertence a uma `SessionId`; encerrar a
//! sessão cancela tudo e apaga tudo, e qualquer trabalho em voo da sessão antiga é
//! descartado em silêncio antes de virar evento. Ver `docs/response-suggestion.md`.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::AppHandle;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::audio::types::AudioSource;
use crate::conversation::{
    ConversationSpeaker, ConversationTimelineEvent, ConversationTurn, ConversationUtterance,
    InternalConversationEventBatch, SessionId, TurnId, UtteranceFinalizationReason, UtteranceId,
};

/// Id de geração, monotônico por processo (nunca reiniciado, nem entre sessões): junto com
/// `SessionId` forma uma identidade que nenhum evento atrasado consegue forjar por
/// coincidência. Serializa como número puro — o frontend continua vendo `generation_id: 3`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct GenerationId(u64);

impl GenerationId {
    pub fn value(self) -> u64 {
        self.0
    }

    #[cfg(test)]
    pub(crate) fn from_raw(value: u64) -> Self {
        Self(value)
    }
}

/// Identidade completa de uma geração. Nenhuma geração existe sem `session_id`: é o campo
/// verificado antes de iniciar a requisição e antes de publicar qualquer evento.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenerationContext {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub utterance_id: UtteranceId,
    pub utterance_revision: u64,
    pub generation_id: GenerationId,
}

/// Todo motivo pelo qual um gatilho de geração automática **não** vira uma geração.
/// Nenhum ponto de decisão retorna em silêncio: cada `return`/`continue` early no
/// caminho do gatilho grava um destes valores via `ResponseEngine::record_rejection`,
/// consultável em modo de desenvolvedor (`response_last_rejection_command`) mesmo quando
/// nenhum evento chega a ser publicado. Nem toda variante tem hoje um ponto de código que
/// a produz — ver `docs/response-suggestion.md`, seção "Motivos de rejeição de geração",
/// para o que é alcançável agora e o que é reservado.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationRejectionReason {
    /// Não há sessão de conversa ativa no motor (encerrando ou nunca iniciada).
    NoActiveSession,
    /// O gatilho pertence a uma sessão diferente da sessão ativa do motor.
    WrongSession,
    /// O transporte interno não conseguiu entregar um evento utilizável ao motor (canal
    /// atrasado ou lote sem o snapshot do turno necessário para avaliar o gatilho).
    EngineNotReady,
    /// O turno da utterance não é de `ConversationSpeaker::OtherPerson`.
    WrongSpeaker,
    /// O turno da utterance não é de `AudioSource::SystemOutput`.
    WrongSource,
    /// Reservado: hoje só `ConversationTimelineEvent::UtteranceFinalized` chega a este
    /// caminho, então uma utterance ainda aberta não tem como disparar o gatilho.
    UtteranceNotFinalized,
    /// Já existe uma geração em andamento para a mesma utterance com uma revisão mais
    /// nova — o gatilho chegou fora de ordem e não pode substituir para trás.
    RevisionMismatch,
    /// Já existe uma geração (em andamento ou recém-concluída) para exatamente a mesma
    /// `(turn_id, utterance_id, utterance_revision)` — reentrega/duplicata do gatilho.
    AlreadyProcessed,
    /// A utterance envelheceu demais entre a finalização e o início da geração
    /// automática (`maximum_automatic_generation_age_ms`).
    StaleInput,
    /// Reservado: a normalização de transcrição roda antes da Conversation Timeline: uma
    /// utterance finalizada sempre carrega `text` (mesmo que a normalização não tenha
    /// mudado nada), então este caminho não tem hoje um produtor real.
    MissingNormalizedText,
    /// Reservado: o motor sempre tem um `ResponseContextBuilder` configurado na
    /// construção; não existe estado runtime em que ele fique ausente.
    MissingGenerationEngine,
    /// Reservado: um provedor mal configurado hoje falha na chamada HTTP em si
    /// (`TerminalState::Error`, depois de `started` já ter sido publicado), não antes do
    /// gatilho — não há hoje uma checagem de disponibilidade prévia ao `started`.
    ProviderUnavailable,
}

impl GenerationRejectionReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::NoActiveSession => "no_active_session",
            Self::WrongSession => "wrong_session",
            Self::EngineNotReady => "engine_not_ready",
            Self::WrongSpeaker => "wrong_speaker",
            Self::WrongSource => "wrong_source",
            Self::UtteranceNotFinalized => "utterance_not_finalized",
            Self::RevisionMismatch => "revision_mismatch",
            Self::AlreadyProcessed => "already_processed",
            Self::StaleInput => "stale_input",
            Self::MissingNormalizedText => "missing_normalized_text",
            Self::MissingGenerationEngine => "missing_generation_engine",
            Self::ProviderUnavailable => "provider_unavailable",
        }
    }
}

/// Último motivo de rejeição observado pelo motor, para modo de desenvolvedor. Não é
/// escopado por sessão de propósito: o objetivo é diagnosticar "por que nada apareceu",
/// inclusive quando a rejeição aconteceu antes de qualquer `session_id` ficar disponível
/// para comparação (ex.: `NoActiveSession`).
#[derive(Debug, Clone, Serialize)]
pub struct GenerationRejectionRecord {
    pub reason: GenerationRejectionReason,
    pub turn_id: Option<u64>,
    pub utterance_id: Option<u64>,
    pub detail: String,
    pub at_epoch_ms: u64,
}

/// Estados terminais possíveis de uma geração. Exatamente um acontece por geração, e
/// apenas os quatro primeiros viram evento para o frontend — `Superseded` já foi
/// anunciado como `cancelled` por quem substituiu a geração, e `SessionEnded` não pode
/// virar evento nenhum, porque a sessão dona daquele evento não existe mais.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminalState {
    Completed,
    Skipped,
    Cancelled,
    Error,
    Invalid,
    Superseded,
}

impl TerminalState {
    fn as_str(self) -> &'static str {
        match self {
            TerminalState::Completed => "completed",
            TerminalState::Skipped => "skipped",
            TerminalState::Cancelled => "cancelled",
            TerminalState::Error => "error",
            TerminalState::Invalid => "invalid",
            TerminalState::Superseded => "superseded",
        }
    }
}

/// Contexto de disparo capturado no momento em que `process_conversation_events` recebe
/// um `UtteranceFinalized` elegível. `utterance_finalized_at` é um `Instant` (não epoch)
/// capturado no início do processamento desse evento — como a finalização (reativa ou
/// pelo timer dedicado) e este processamento acontecem na mesma pilha de chamada
/// síncrona, isso equivale, na prática, ao instante real de finalização, sem precisar
/// serializar `Instant` através do evento da timeline.
pub struct GenerationTrigger {
    pub session_id: SessionId,
    pub utterance_id: UtteranceId,
    pub utterance_revision: u64,
    /// Texto **só da utterance** que acabou de finalizar — a "fala atual". Separado do
    /// texto do turno de propósito: o turno acumula tudo que a pessoa disse enquanto teve
    /// a palavra, e mandar isso como "fala mais recente" fazia o modelo decidir sobre uma
    /// pergunta velha (ou responder `[SKIP]` por já ter "visto" tudo aquilo).
    pub utterance_text: String,
    /// Snapshot da utterance finalizada, incluindo texto bruto apenas para validacao.
    pub utterance: ConversationUtterance,
    pub utterance_finalized_at: Instant,
    pub speech_ended_at: Instant,
    /// Regeneracao manual pode ignorar o limite de idade de entrada.
    pub automatic: bool,
    pub finalization_reason: String,
    pub gap_ms_used: u64,
    pub silence_detected_ms: Option<u64>,
}

use super::anthropic::AnthropicProvider;
use super::config_store::{self, ResponseProviderConfig, ResponseProviderKind};
use super::context::{
    snapshot_generation_request, DefaultResponseContextBuilder, ResponseContextBuilder,
    ResponseGenerationRequest,
};
use super::echo_guard::EchoGuard;
use super::events::{
    emit_response_suggestion_event, GenerationDiagnostics, ResponseSuggestionEvent,
};
use super::ollama::OllamaProvider;
use super::openai_compatible::OpenAiCompatibleProvider;
use super::provider::{
    ResponseChunk, ResponseProvider, ResponseProviderCapabilities, ResponseProviderError,
    ResponseRequest,
};
use super::secrets;
use super::skip_detector::{SkipDecision, SkipDetector};
use super::validation::{
    validate_suggestion, SuggestionValidation, SuggestionValidationFailure, ValidatedSuggestion,
};

const MAX_HISTORY_TURNS: usize = 20;

/// Motivo de cancelamento hoje é sempre este: uma nova utterance no mesmo turno
/// substituiu a geração em andamento (ver `trigger_generation`).
const CANCEL_REASON_NEW_UTTERANCE: &str = "new_utterance";

/// Quantos caracteres brutos (antes do `SkipDetector`) manter para diagnóstico. Cobre
/// folgadamente o marcador `[SKIP]`, sem acumular texto de respostas longas à toa.
const RAW_PREFIX_CAP_CHARS: usize = 80;
#[cfg(test)]
const MIN_USEFUL_RESPONSE_CHARS: usize = 12;

#[cfg(test)]
fn normalize_generated_response_for_quality(text: &str) -> String {
    text.chars()
        .map(|ch| {
            if ch.is_alphanumeric() {
                ch.to_lowercase().collect::<String>()
            } else {
                " ".to_string()
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
fn is_invalid_generated_response(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return true;
    }

    if trimmed
        .chars()
        .all(|ch| ch.is_whitespace() || ch.is_ascii_punctuation())
    {
        return true;
    }

    let normalized = normalize_generated_response_for_quality(trimmed);
    if normalized.is_empty() {
        return true;
    }

    let useful_len = normalized.chars().filter(|ch| ch.is_alphanumeric()).count();
    let low_value = [
        "sim",
        "não",
        "nao",
        "tchau",
        "até logo",
        "ate logo",
        "obrigado",
        "valeu",
    ];
    low_value.contains(&normalized.as_str())
        || (useful_len < MIN_USEFUL_RESPONSE_CHARS && normalized.starts_with("tchau"))
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn truncate_for_diagnostics(text: &str, maximum_characters: usize) -> String {
    if text.chars().count() <= maximum_characters {
        text.to_string()
    } else {
        text.chars().take(maximum_characters).collect()
    }
}

struct GenerationHandle {
    context: GenerationContext,
    cancel: CancellationToken,
    /// Marcado na primeira vez que um estado terminal é publicado para esta geração.
    /// Compartilhado com a task que roda a geração, para que nem a substituição
    /// (`cancelled` emitido por quem substituiu) nem a própria task publiquem um segundo
    /// estado terminal.
    terminal_emitted: Arc<AtomicBool>,
}

/// Todo o estado conversacional do motor, sempre atrás de um único mutex para que
/// encerrar a sessão seja atômico: não existe instante em que a sessão já mudou mas o
/// histórico antigo ainda está lá (ou vice-versa).
struct SessionState {
    session_id: SessionId,
    /// Ligado no início do encerramento, antes de qualquer limpeza: bloqueia gatilhos
    /// novos e publicação de eventos enquanto a sessão está sendo desmontada.
    ending: bool,
    /// Token raiz da sessão. Cada geração recebe um `child_token()` dele, então cancelar
    /// a sessão cancela todas as gerações em voo de uma vez. Nunca é reutilizado depois de
    /// cancelado: `begin_session` instala um token novo.
    cancel: CancellationToken,
    /// Histórico rolante usado para montar o prompt — **por sessão**. Era isto que
    /// sobrevivia ao encerramento e fazia perguntas da sessão anterior reaparecerem.
    history: VecDeque<ConversationUtterance>,
    generations: HashMap<UtteranceId, GenerationHandle>,
    last_suggestion: Option<String>,
}

impl SessionState {
    fn new(session_id: SessionId) -> Self {
        SessionState {
            session_id,
            ending: false,
            cancel: CancellationToken::new(),
            history: VecDeque::with_capacity(MAX_HISTORY_TURNS),
            generations: HashMap::new(),
            last_suggestion: None,
        }
    }
}

struct MisconfiguredProvider {
    message: String,
}

#[async_trait::async_trait]
impl ResponseProvider for MisconfiguredProvider {
    fn id(&self) -> super::provider::ResponseProviderId {
        super::provider::ResponseProviderId::Misconfigured
    }

    fn capabilities(&self) -> super::provider::ResponseProviderCapabilities {
        super::provider::ResponseProviderCapabilities::none()
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

/// Lê a chave do keychain para um provedor que **exige** credencial. `Ok(None)` e erro de
/// keychain viram a mesma coisa aqui — uma mensagem — porque o efeito para o usuário é o
/// mesmo: não há como gerar, e ele precisa saber por quê antes de entrar numa reunião.
fn required_api_key(kind: ResponseProviderKind) -> Result<String, String> {
    match secrets::load_api_key(kind) {
        Ok(Some(key)) => Ok(key),
        Ok(None) => Err(format!(
            "nenhuma API key de {} configurada",
            kind.id().display_name()
        )),
        Err(e) => Err(format!(
            "falha ao ler a API key de {}: {e}",
            kind.id().display_name()
        )),
    }
}

/// Chave opcional: LM Studio e endpoint personalizado funcionam sem nenhuma. Uma falha de
/// keychain aqui não é fatal — vira `None` com aviso, e a configuração decide se isso é um
/// problema (via `CredentialMode`).
fn optional_api_key(kind: ResponseProviderKind) -> Option<String> {
    match secrets::load_api_key(kind) {
        Ok(key) => key,
        Err(e) => {
            tracing::warn!(
                provider = kind.id().as_str(),
                %e,
                "falha ao ler credencial opcional do keychain; seguindo sem credencial"
            );
            None
        }
    }
}

fn build_provider(config: &ResponseProviderConfig) -> Arc<dyn ResponseProvider> {
    // Toda construção compatível com a API da OpenAI pode falhar por endpoint recusado
    // (esquema inválido, credencial embutida na URL) ou cabeçalho reservado. A falha vira
    // `MisconfiguredProvider` com a mensagem, nunca um provider silenciosamente quebrado.
    fn from_openai_compatible(
        built: Result<OpenAiCompatibleProvider, ResponseProviderError>,
    ) -> Arc<dyn ResponseProvider> {
        match built {
            Ok(provider) => Arc::new(provider),
            Err(e) => misconfigured(&e.to_string()),
        }
    }

    match config.provider {
        ResponseProviderKind::Ollama => Arc::new(OllamaProvider::new(
            config.base_url.clone(),
            config.model.clone(),
            config.ollama_keep_alive.clone(),
        )),
        ResponseProviderKind::LmStudio => {
            from_openai_compatible(OpenAiCompatibleProvider::lm_studio(
                config.model.clone(),
                config.base_url.clone(),
                optional_api_key(ResponseProviderKind::LmStudio),
            ))
        }
        ResponseProviderKind::OpenAi => match required_api_key(ResponseProviderKind::OpenAi) {
            Ok(api_key) => from_openai_compatible(OpenAiCompatibleProvider::openai(
                api_key,
                config.model.clone(),
                config.base_url.clone(),
            )),
            Err(message) => misconfigured(&message),
        },
        ResponseProviderKind::DeepSeek => match required_api_key(ResponseProviderKind::DeepSeek) {
            Ok(api_key) => from_openai_compatible(OpenAiCompatibleProvider::deepseek(
                api_key,
                config.model.clone(),
                config.base_url.clone(),
            )),
            Err(message) => misconfigured(&message),
        },
        ResponseProviderKind::OpenRouter => {
            match required_api_key(ResponseProviderKind::OpenRouter) {
                Ok(api_key) => from_openai_compatible(OpenAiCompatibleProvider::openrouter(
                    api_key,
                    config.model.clone(),
                    config.base_url.clone(),
                )),
                Err(message) => misconfigured(&message),
            }
        }
        ResponseProviderKind::CustomOpenAiCompatible => {
            let Some(base_url) = config.base_url.clone() else {
                // Sem URL não há para onde cair de volta: adivinhar um endpoint aqui
                // mandaria a conversa da reunião para um host que o usuário não escolheu.
                return misconfigured(
                    "endpoint compatível com a OpenAI exige uma URL base configurada",
                );
            };
            from_openai_compatible(OpenAiCompatibleProvider::custom(
                base_url,
                config.model.clone(),
                optional_api_key(ResponseProviderKind::CustomOpenAiCompatible),
                config.credential_mode,
                config.custom_headers.clone(),
            ))
        }
        ResponseProviderKind::Anthropic => {
            match required_api_key(ResponseProviderKind::Anthropic) {
                Ok(api_key) => Arc::new(AnthropicProvider::new(
                    api_key,
                    config.model.clone(),
                    config.base_url.clone(),
                )),
                Err(message) => misconfigured(&message),
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
    /// Montagem de prompt como dependência, não como chamada direta a uma função livre.
    /// Não é `Mutex` porque não há caminho de troca em runtime: o builder é escolhido na
    /// construção do motor. O ganho é de isolamento — quem monta contexto vê apenas
    /// `ResponseContextInput` e não tem acesso ao estado de sessão, às gerações ativas ou
    /// aos diagnósticos.
    context_builder: Arc<dyn ResponseContextBuilder>,
    config: Mutex<ResponseProviderConfig>,
    config_path: PathBuf,
    session: Mutex<SessionState>,
    next_generation_id: AtomicU64,
    /// Último motivo de rejeição de um gatilho automático, para diagnóstico em modo de
    /// desenvolvedor. Deliberadamente fora do `Mutex<SessionState>`: uma rejeição por
    /// `NoActiveSession`/`WrongSession` acontece justamente quando não há uma sessão
    /// coerente para amarrar o registro.
    last_rejection: Mutex<Option<GenerationRejectionRecord>>,
}

impl ResponseEngine {
    /// A sessão inicial é um placeholder: `lib.rs` chama `begin_session` com a sessão real
    /// do `ConversationTimeline` logo depois de construir os dois, e todo gatilho é
    /// comparado com ela. Enquanto isso não acontece, nenhum gatilho é aceito — preferível
    /// a adotar silenciosamente a sessão de quem chamar primeiro.
    pub fn from_config_path(config_path: PathBuf) -> Self {
        let config = config_store::load(&config_path);
        let provider = build_provider(&config);
        ResponseEngine {
            provider: Mutex::new(provider),
            context_builder: Arc::new(DefaultResponseContextBuilder),
            config: Mutex::new(config),
            config_path,
            session: Mutex::new(SessionState::new(SessionId::new())),
            next_generation_id: AtomicU64::new(0),
            last_rejection: Mutex::new(None),
        }
    }

    /// Grava o motivo de uma rejeição e loga em `warn` — o ponto único que garante que
    /// nenhum `return` early do caminho de gatilho automático é silencioso (spec: "nunca
    /// faça return silencioso").
    fn record_rejection(
        &self,
        reason: GenerationRejectionReason,
        turn_id: Option<TurnId>,
        utterance_id: Option<UtteranceId>,
        detail: impl Into<String>,
    ) {
        let detail = detail.into();
        tracing::warn!(
            reason = reason.as_str(),
            turn_id = turn_id.map(|id| id.value()),
            utterance_id = utterance_id.map(|id| id.value()),
            detail = %detail,
            "response_engine_trigger_rejected"
        );
        let mut last_rejection = self
            .last_rejection
            .lock()
            .expect("response engine mutex poisoned");
        *last_rejection = Some(GenerationRejectionRecord {
            reason,
            turn_id: turn_id.map(|id| id.value()),
            utterance_id: utterance_id.map(|id| id.value()),
            detail,
            at_epoch_ms: epoch_ms(),
        });
    }

    /// Último motivo de rejeição de geração automática — para o painel de modo de
    /// desenvolvedor mostrar "por que nada apareceu" mesmo quando nenhum evento de
    /// sugestão chegou a ser publicado.
    pub fn last_rejection(&self) -> Option<GenerationRejectionRecord> {
        self.last_rejection
            .lock()
            .expect("response engine mutex poisoned")
            .clone()
    }

    /// Sessão ativa do motor. Deve espelhar `ConversationTimeline::session_id`; as duas só
    /// mudam juntas, nos comandos de início/fim de sessão. Só os testes precisam ler isso
    /// diretamente — o código de produção compara sessões pelos caminhos que já validam
    /// (`push_history`, `history_snapshot`, `is_publishable`, `session_is_active`).
    #[cfg(test)]
    pub fn active_session_id(&self) -> SessionId {
        self.session
            .lock()
            .expect("response engine mutex poisoned")
            .session_id
    }

    /// Instala uma sessão nova e vazia: token raiz novo (nunca um já cancelado), histórico
    /// vazio, nenhuma geração ativa. Provider e `reqwest::Client` são deliberadamente
    /// preservados — reaproveitar conexão é correto, reaproveitar conversa não.
    pub fn begin_session(&self, session_id: SessionId) {
        let mut state = self.session.lock().expect("response engine mutex poisoned");
        let previous = state.session_id;
        *state = SessionState::new(session_id);
        tracing::info!(
            session_id = session_id.value(),
            previous_session_id = previous.value(),
            "session_started"
        );
    }

    /// Encerramento atômico e ordenado da sessão: marca como encerrando (nenhum gatilho
    /// novo passa a partir daqui), cancela o token raiz (e com ele toda geração em voo),
    /// marca as gerações ativas como já terminadas (para que a task, ao acordar, não
    /// publique `cancelled`/`completed`/`error` de uma sessão que não existe mais) e apaga
    /// o histórico usado no prompt. Idempotente: chamar com uma sessão que não é a ativa,
    /// ou duas vezes seguidas, é um no-op registrado em log.
    pub fn end_session(&self, session_id: SessionId) {
        let mut state = self.session.lock().expect("response engine mutex poisoned");
        if state.session_id != session_id {
            tracing::debug!(
                requested_session_id = session_id.value(),
                active_session_id = state.session_id.value(),
                "session end ignored: not the active session"
            );
            return;
        }
        if state.ending {
            tracing::debug!(
                session_id = session_id.value(),
                "session end ignored: already ending"
            );
            return;
        }
        tracing::info!(
            session_id = session_id.value(),
            active_generations = state.generations.len(),
            history_turns = state.history.len(),
            "session_ending"
        );
        state.ending = true;
        state.cancel.cancel();
        for (_utterance_id, handle) in state.generations.drain() {
            // `swap` e não `store`: se a geração já tinha publicado seu estado terminal,
            // não há nada a cancelar — só evita que ela publique um segundo.
            let already_terminal = handle.terminal_emitted.swap(true, Ordering::SeqCst);
            handle.cancel.cancel();
            if !already_terminal {
                tracing::info!(
                    session_id = session_id.value(),
                    turn_id = handle.context.turn_id.value(),
                    generation_id = handle.context.generation_id.value(),
                    "generation_cancelled_session_end"
                );
            }
        }
        let cleared_turns = state.history.len();
        state.history.clear();
        tracing::info!(
            session_id = session_id.value(),
            cleared_turns,
            "session_state_cleared"
        );
    }

    pub fn current_config(&self) -> ResponseProviderConfig {
        self.config
            .lock()
            .expect("response engine mutex poisoned")
            .clone()
    }

    /// Capacidades do provedor **efetivamente construído**, não as do catálogo. A diferença
    /// importa: `registry::descriptors()` descreve o LM Studio como local porque o padrão
    /// dele é `localhost`, mas a instância sabe se o `base_url` configurado aponta para
    /// outra máquina. Um provedor mal configurado responde `none()` e a UI mostra isso em
    /// vez de prometer streaming que não vai acontecer.
    pub fn active_capabilities(&self) -> ResponseProviderCapabilities {
        self.provider
            .lock()
            .expect("response engine mutex poisoned")
            .capabilities()
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

    /// Só aceita turnos da sessão ativa. Um turno finalizado que chegue atrasado, de uma
    /// sessão já encerrada, é descartado — é ele que, antes, entrava no histórico e
    /// contaminava o prompt da sessão seguinte.
    fn push_history(&self, session_id: SessionId, utterance: ConversationUtterance) {
        let mut state = self.session.lock().expect("response engine mutex poisoned");
        if state.session_id != session_id || state.ending {
            tracing::debug!(
                turn_session_id = session_id.value(),
                active_session_id = state.session_id.value(),
                utterance_id = utterance.id.value(),
                "history turn discarded: not the active session"
            );
            return;
        }
        state.history.retain(|existing| existing.id != utterance.id);
        state.history.push_back(utterance);
        while state.history.len() > MAX_HISTORY_TURNS {
            state.history.pop_front();
        }
    }

    /// Histórico da sessão `session_id`, ou `None` se ela não for mais a sessão ativa —
    /// nunca devolve um histórico "global".
    #[cfg(test)]
    fn history_snapshot(&self, session_id: SessionId) -> Option<Vec<ConversationUtterance>> {
        let state = self.session.lock().expect("response engine mutex poisoned");
        if state.session_id != session_id || state.ending {
            return None;
        }
        Some(state.history.iter().cloned().collect())
    }

    /// Publica um evento de streaming (`started`/`delta`). Silenciosamente descartado —
    /// com log em `debug` — se a sessão mudou, está encerrando, ou a geração já foi
    /// substituída/cancelada.
    fn publish_stream_event<R: tauri::Runtime>(
        &self,
        app: &AppHandle<R>,
        ctx: &GenerationContext,
        event: ResponseSuggestionEvent,
    ) -> bool {
        let state = self.session.lock().expect("response engine mutex poisoned");
        let publishable = state.session_id == ctx.session_id
            && !state.ending
            && !state.cancel.is_cancelled()
            && state
                .generations
                .get(&ctx.utterance_id)
                .is_some_and(|handle| handle.context == *ctx);
        if !publishable {
            tracing::debug!(
                session_id = ctx.session_id.value(),
                turn_id = ctx.turn_id.value(),
                generation_id = ctx.generation_id.value(),
                "generation_event_discarded_stale"
            );
            return false;
        }
        // O lock permanece vivo ate o emit: nenhuma supersessao pode entrar entre a
        // validacao da identidade e a publicacao.
        emit_response_suggestion_event(app, event);
        true
    }

    /// Publica **o** estado terminal da geração, no máximo uma vez (`terminal_emitted`
    /// funciona como trava de dupla finalização) e só se a sessão ainda for a ativa.
    ///
    /// Diferente de `publish_stream_event`, não exige que a geração ainda esteja
    /// registrada em `state.generations`: uma geração superseded é removida do mapa
    /// pelo chamador (`trigger_generation`) antes de publicar o `cancelled` dela — exigir
    /// presença no mapa aqui tornaria esse `cancelled` estruturalmente impossível de
    /// publicar. A trava real contra dupla finalização é `terminal_emitted`.
    fn publish_terminal_event<R: tauri::Runtime>(
        &self,
        app: &AppHandle<R>,
        ctx: &GenerationContext,
        terminal_emitted: &AtomicBool,
        terminal: TerminalState,
        event: Option<ResponseSuggestionEvent>,
    ) -> bool {
        let state = self.session.lock().expect("response engine mutex poisoned");
        let publishable = state.session_id == ctx.session_id && !state.ending;
        if !publishable {
            tracing::debug!(
                session_id = ctx.session_id.value(),
                turn_id = ctx.turn_id.value(),
                utterance_id = ctx.utterance_id.value(),
                utterance_revision = ctx.utterance_revision,
                generation_id = ctx.generation_id.value(),
                terminal_state = terminal.as_str(),
                "generation_event_discarded_stale"
            );
            return false;
        }
        if terminal_emitted.swap(true, Ordering::SeqCst) {
            tracing::debug!(
                session_id = ctx.session_id.value(),
                turn_id = ctx.turn_id.value(),
                generation_id = ctx.generation_id.value(),
                terminal_state = terminal.as_str(),
                "generation_event_discarded_stale: terminal state already emitted"
            );
            return false;
        }
        tracing::info!(
            session_id = ctx.session_id.value(),
            turn_id = ctx.turn_id.value(),
            generation_id = ctx.generation_id.value(),
            terminal_state = terminal.as_str(),
            "terminal_state"
        );
        if let Some(event) = event {
            emit_response_suggestion_event(app, event);
        }
        true
    }

    fn clear_if_current(&self, ctx: &GenerationContext) {
        let mut state = self.session.lock().expect("response engine mutex poisoned");
        if state.session_id != ctx.session_id {
            return;
        }
        if state
            .generations
            .get(&ctx.utterance_id)
            .map(|handle| handle.context)
            == Some(*ctx)
        {
            state.generations.remove(&ctx.utterance_id);
        }
    }

    fn snapshot_at_trigger(
        &self,
        turn_id: TurnId,
        generation_id: GenerationId,
        trigger: &GenerationTrigger,
    ) -> Option<ResponseGenerationRequest> {
        let state = self.session.lock().expect("response engine mutex poisoned");
        if state.ending {
            drop(state);
            self.record_rejection(
                GenerationRejectionReason::NoActiveSession,
                Some(turn_id),
                Some(trigger.utterance_id),
                "session is ending",
            );
            return None;
        }
        if state.session_id != trigger.session_id {
            let active_session_id = state.session_id;
            drop(state);
            self.record_rejection(
                GenerationRejectionReason::WrongSession,
                Some(turn_id),
                Some(trigger.utterance_id),
                format!(
                    "trigger session {} != active session {}",
                    trigger.session_id.value(),
                    active_session_id.value()
                ),
            );
            return None;
        }
        let history: Vec<_> = state.history.iter().cloned().collect();
        Some(snapshot_generation_request(
            trigger.session_id,
            turn_id,
            &trigger.utterance,
            generation_id,
            &history,
            state.last_suggestion.clone(),
            None,
            Instant::now(),
            trigger.speech_ended_at,
            trigger.automatic,
        ))
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
        let generation_id =
            GenerationId(self.next_generation_id.fetch_add(1, Ordering::Relaxed) + 1);
        let Some(request) = self.snapshot_at_trigger(turn.id, generation_id, &trigger) else {
            return;
        };
        let ctx = GenerationContext {
            session_id: request.session_id,
            turn_id: request.turn_id,
            utterance_id: request.utterance_id,
            utterance_revision: request.utterance_revision,
            generation_id: request.generation_id,
        };
        let terminal_emitted = Arc::new(AtomicBool::new(false));

        // Reserva do slot, validação de sessão e cancelamento do que estava lá acontecem
        // sob o mesmo lock: não existe janela em que a sessão acabou de encerrar e este
        // gatilho ainda consegue registrar uma geração.
        let (cancel_token, superseded) = {
            let mut state = self.session.lock().expect("response engine mutex poisoned");
            if state.session_id != ctx.session_id {
                let active_session_id = state.session_id;
                drop(state);
                self.record_rejection(
                    GenerationRejectionReason::WrongSession,
                    Some(ctx.turn_id),
                    Some(ctx.utterance_id),
                    format!(
                        "trigger session {} != active session {}",
                        ctx.session_id.value(),
                        active_session_id.value()
                    ),
                );
                return;
            }
            if state.ending {
                drop(state);
                self.record_rejection(
                    GenerationRejectionReason::NoActiveSession,
                    Some(ctx.turn_id),
                    Some(ctx.utterance_id),
                    "session is ending",
                );
                return;
            }
            // Reentrega/duplicata do mesmo gatilho (mesma chave `turn_id + utterance_id +
            // utterance_revision`), ou um gatilho fora de ordem para uma revisão mais
            // velha que a que já está em andamento: nenhum dos dois pode substituir a
            // geração já registrada para esta utterance.
            if let Some(existing) = state.generations.get(&ctx.utterance_id) {
                let existing_generation_id = existing.context.generation_id;
                let existing_revision = existing.context.utterance_revision;
                if existing_revision == ctx.utterance_revision {
                    drop(state);
                    self.record_rejection(
                        GenerationRejectionReason::AlreadyProcessed,
                        Some(ctx.turn_id),
                        Some(ctx.utterance_id),
                        format!(
                            "generation {} already tracks utterance_revision {}",
                            existing_generation_id.value(),
                            ctx.utterance_revision
                        ),
                    );
                    return;
                }
                if existing_revision > ctx.utterance_revision {
                    drop(state);
                    self.record_rejection(
                        GenerationRejectionReason::RevisionMismatch,
                        Some(ctx.turn_id),
                        Some(ctx.utterance_id),
                        format!(
                            "trigger revision {} is older than tracked revision {}",
                            ctx.utterance_revision, existing_revision
                        ),
                    );
                    return;
                }
            }
            let cancel_token = state.cancel.child_token();
            let previous_key = state.generations.iter().find_map(|(utterance_id, handle)| {
                (handle.context.turn_id == ctx.turn_id).then_some(*utterance_id)
            });
            let previous = previous_key.and_then(|key| state.generations.remove(&key));
            state.generations.insert(
                ctx.utterance_id,
                GenerationHandle {
                    context: ctx,
                    cancel: cancel_token.clone(),
                    terminal_emitted: terminal_emitted.clone(),
                },
            );
            (cancel_token, previous)
        };

        if let Some(previous) = superseded {
            previous.cancel.cancel();
            // A geração substituída termina aqui, em `cancelled`: sua própria task, ao
            // acordar cancelada, encontra `terminal_emitted` já marcado e não publica um
            // segundo estado terminal.
            let previous_ctx = previous.context;
            self.publish_terminal_event(
                &app,
                &previous_ctx,
                &previous.terminal_emitted,
                TerminalState::Superseded,
                Some(ResponseSuggestionEvent::Cancelled {
                    session_id: previous_ctx.session_id,
                    turn_id: previous_ctx.turn_id,
                    utterance_id: previous_ctx.utterance_id,
                    utterance_revision: previous_ctx.utterance_revision,
                    generation_id: previous_ctx.generation_id,
                }),
            );
        }

        let engine = self.clone();
        tauri::async_runtime::spawn(async move {
            engine
                .run_generation(app, ctx, terminal_emitted, cancel_token, trigger, request)
                .await;
        });
    }

    async fn run_generation<R: tauri::Runtime>(
        self: Arc<Self>,
        app: AppHandle<R>,
        ctx: GenerationContext,
        terminal_emitted: Arc<AtomicBool>,
        cancel_token: CancellationToken,
        trigger: GenerationTrigger,
        request: ResponseGenerationRequest,
    ) {
        let provider = self
            .provider
            .lock()
            .expect("response engine mutex poisoned")
            .clone();
        let config = self.current_config();
        let started_at = Instant::now();
        let utterance_age_at_generation_start_ms = request
            .speech_ended_at
            .elapsed()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;

        if request.automatic
            && utterance_age_at_generation_start_ms > config.maximum_automatic_generation_age_ms
        {
            self.clear_if_current(&ctx);
            self.record_rejection(
                GenerationRejectionReason::StaleInput,
                Some(ctx.turn_id),
                Some(ctx.utterance_id),
                format!(
                    "utterance_age_ms {} > maximum_age_ms {}",
                    utterance_age_at_generation_start_ms,
                    config.maximum_automatic_generation_age_ms
                ),
            );
            return;
        }

        let built = self.context_builder.build(&request);
        tracing::info!(
            session_id = ctx.session_id.value(),
            turn_id = ctx.turn_id.value(),
            utterance_id = ctx.utterance_id.value(),
            revision = ctx.utterance_revision,
            generation_id = ctx.generation_id.value(),
            provider = provider.provider_name(),
            model = %config.model,
            context_turn_count = built.context_turn_count,
            context_character_count = built.context_character_count,
            "response generation request built"
        );
        let trigger_text_hash = format!(
            "{:x}",
            Sha256::digest(request.current_remote_utterance.as_bytes())
        );
        let now_epoch = epoch_ms();
        let utterance_finalized_epoch = now_epoch.saturating_sub(
            trigger
                .utterance_finalized_at
                .elapsed()
                .as_millis()
                .min(u128::from(u64::MAX)) as u64,
        );
        let speech_ended_epoch = now_epoch.saturating_sub(utterance_age_at_generation_start_ms);
        let generation_triggered_epoch = now_epoch.saturating_sub(
            request
                .created_at
                .elapsed()
                .as_millis()
                .min(u128::from(u64::MAX)) as u64,
        );
        let mut diagnostics = GenerationDiagnostics {
            session_id: ctx.session_id,
            generation_id: ctx.generation_id,
            turn_id: ctx.turn_id,
            utterance_id: ctx.utterance_id,
            utterance_revision: ctx.utterance_revision,
            provider: provider.provider_name().to_string(),
            model: config.model,
            trigger_text: truncate_for_diagnostics(&request.current_remote_utterance, 240),
            trigger_text_hash,
            context_utterance_ids: built.context_utterance_ids.clone(),
            context_turn_count: built.context_turn_count,
            context_character_count: built.context_character_count,
            prompt_preview: built.sanitized_preview.clone(),
            speech_ended_at: speech_ended_epoch,
            transcription_completed_at: Some(
                now_epoch.saturating_sub(
                    request
                        .transcription_completed_at
                        .elapsed()
                        .as_millis()
                        .min(u128::from(u64::MAX)) as u64,
                ),
            ),
            utterance_finalized_at: utterance_finalized_epoch,
            generation_triggered_at: generation_triggered_epoch,
            request_started: 0,
            first_visible_token_at: None,
            completed_at: 0,
            utterance_age_at_generation_start_ms,
            utterance_age_at_first_token_ms: None,
            http_status: None,
            first_chunk_received: None,
            raw_prefix: String::new(),
            skip_detected: false,
            echo_suppressed_characters: 0,
            validation_result: "pending".to_string(),
            retry_used: false,
            context_leak_score: 0.0,
            cancel_reason: None,
            latency_ms: 0,
            final_text_length: 0,
            event_emitted: "started".to_string(),
            terminal_state: "running".to_string(),
            finalization_reason: trigger.finalization_reason.clone(),
            gap_ms_used: trigger.gap_ms_used,
            silence_detected_ms: trigger.silence_detected_ms,
            utterance_finalized_to_request_started_ms: None,
            request_to_first_http_chunk_ms: None,
            request_to_first_visible_token_ms: None,
            end_of_speech_to_first_visible_token_ms: None,
        };

        let request_started_at = Instant::now();
        diagnostics.request_started = epoch_ms();
        let mut first_http_chunk_at = None;
        let mut first_visible_text_at = None;

        macro_rules! finish {
            () => {{
                diagnostics.completed_at = epoch_ms();
                self.finish_generation(
                    &app,
                    &ctx,
                    diagnostics,
                    started_at,
                    trigger.utterance_finalized_at,
                    request_started_at,
                    first_http_chunk_at,
                    first_visible_text_at,
                );
                return;
            }};
        }

        let published = self.publish_stream_event(
            &app,
            &ctx,
            ResponseSuggestionEvent::Started {
                session_id: ctx.session_id,
                turn_id: ctx.turn_id,
                utterance_id: ctx.utterance_id,
                utterance_revision: ctx.utterance_revision,
                generation_id: ctx.generation_id,
            },
        );
        if !published {
            diagnostics.event_emitted = "discarded_stale".to_string();
            diagnostics.terminal_state = "discarded_stale".to_string();
            finish!();
        }

        tracing::info!(
            provider = provider.provider_name(),
            session_id = ctx.session_id.value(),
            turn_id = ctx.turn_id.value(),
            utterance_id = ctx.utterance_id.value(),
            revision = ctx.utterance_revision,
            generation_id = ctx.generation_id.value(),
            "starting response generation"
        );

        for attempt in 0..=1 {
            let attempt_context = if attempt == 0 {
                built.clone()
            } else {
                diagnostics.retry_used = true;
                self.context_builder.build_repair(&request)
            };
            let stream_result = tokio::select! {
                _ = cancel_token.cancelled() => {
                    diagnostics.cancel_reason = Some(CANCEL_REASON_NEW_UTTERANCE.to_string());
                    diagnostics.event_emitted = "cancelled".to_string();
                    diagnostics.terminal_state = "cancelled".to_string();
                    self.publish_terminal_event(
                        &app,
                        &ctx,
                        &terminal_emitted,
                        TerminalState::Cancelled,
                        Some(ResponseSuggestionEvent::Cancelled {
                            session_id: ctx.session_id,
                            turn_id: ctx.turn_id,
                            utterance_id: ctx.utterance_id,
                            utterance_revision: ctx.utterance_revision,
                            generation_id: ctx.generation_id,
                        }),
                    );
                    finish!();
                }
                result = provider.stream_reply(attempt_context.request) => result,
            };

            let mut stream = match stream_result {
                Ok((stream, meta)) => {
                    diagnostics.http_status = Some(meta.http_status);
                    stream
                }
                Err(error) => {
                    diagnostics.event_emitted = "error".to_string();
                    diagnostics.terminal_state = "error".to_string();
                    self.publish_terminal_event(
                        &app,
                        &ctx,
                        &terminal_emitted,
                        TerminalState::Error,
                        Some(ResponseSuggestionEvent::Error {
                            session_id: ctx.session_id,
                            turn_id: ctx.turn_id,
                            utterance_id: ctx.utterance_id,
                            utterance_revision: ctx.utterance_revision,
                            generation_id: ctx.generation_id,
                            message: error.to_string(),
                        }),
                    );
                    finish!();
                }
            };

            let mut raw_output = String::new();
            loop {
                let next = tokio::select! {
                    _ = cancel_token.cancelled() => {
                        diagnostics.cancel_reason = Some(CANCEL_REASON_NEW_UTTERANCE.to_string());
                        diagnostics.event_emitted = "cancelled".to_string();
                        diagnostics.terminal_state = "cancelled".to_string();
                        self.publish_terminal_event(
                            &app,
                            &ctx,
                            &terminal_emitted,
                            TerminalState::Cancelled,
                            Some(ResponseSuggestionEvent::Cancelled {
                                session_id: ctx.session_id,
                                turn_id: ctx.turn_id,
                                utterance_id: ctx.utterance_id,
                                utterance_revision: ctx.utterance_revision,
                                generation_id: ctx.generation_id,
                            }),
                        );
                        finish!();
                    }
                    item = stream.next() => item,
                };
                let Some(item) = next else { break };
                if first_http_chunk_at.is_none() {
                    first_http_chunk_at = Some(Instant::now());
                    diagnostics.first_chunk_received = Some(epoch_ms());
                }
                match item {
                    Ok(ResponseChunk::Delta(text)) => raw_output.push_str(&text),
                    Ok(ResponseChunk::Done) => break,
                    Err(error) => {
                        diagnostics.event_emitted = "error".to_string();
                        diagnostics.terminal_state = "error".to_string();
                        self.publish_terminal_event(
                            &app,
                            &ctx,
                            &terminal_emitted,
                            TerminalState::Error,
                            Some(ResponseSuggestionEvent::Error {
                                session_id: ctx.session_id,
                                turn_id: ctx.turn_id,
                                utterance_id: ctx.utterance_id,
                                utterance_revision: ctx.utterance_revision,
                                generation_id: ctx.generation_id,
                                message: error.to_string(),
                            }),
                        );
                        finish!();
                    }
                }
            }

            diagnostics.raw_prefix = raw_output.chars().take(RAW_PREFIX_CAP_CHARS).collect();
            let mut skip_detector = SkipDetector::new();
            let skip_decision = match skip_detector.push(&raw_output) {
                SkipDecision::Pending => skip_detector.finish(),
                decision => decision,
            };
            let trimmed = raw_output.trim();
            let candidate = if matches!(skip_decision, SkipDecision::Skip)
                && trimmed.eq_ignore_ascii_case(super::skip_detector::SKIP_MARKER)
            {
                trimmed.to_string()
            } else {
                let mut echo_guard = EchoGuard::new(&request.current_remote_utterance);
                let mut guarded = echo_guard.push(&raw_output);
                guarded.push_str(&echo_guard.finish());
                diagnostics.echo_suppressed_characters = raw_output
                    .chars()
                    .count()
                    .saturating_sub(guarded.chars().count());
                guarded
            };
            let mut leak_references = request.context_leak_references.clone();
            if let Some(previous) = request.previous_suggestion.as_ref() {
                if !leak_references.iter().any(|value| value == previous) {
                    leak_references.push(previous.clone());
                }
            }
            let candidate_is_full_echo =
                candidate.trim().is_empty() && diagnostics.echo_suppressed_characters > 0;
            let validation = if candidate_is_full_echo {
                SuggestionValidation {
                    result: Err(SuggestionValidationFailure::EchoOfQuestion),
                    context_leak_score: 1.0,
                }
            } else {
                validate_suggestion(
                    &candidate,
                    &request.current_remote_utterance,
                    &leak_references,
                )
            };
            diagnostics.context_leak_score = validation.context_leak_score;

            match validation.result {
                Ok(ValidatedSuggestion::Skip) => {
                    diagnostics.skip_detected = true;
                    diagnostics.validation_result = "valid_skip".to_string();
                    diagnostics.event_emitted = "skipped".to_string();
                    diagnostics.terminal_state = "skipped".to_string();
                    self.publish_terminal_event(
                        &app,
                        &ctx,
                        &terminal_emitted,
                        TerminalState::Skipped,
                        Some(ResponseSuggestionEvent::Skipped {
                            session_id: ctx.session_id,
                            turn_id: ctx.turn_id,
                            utterance_id: ctx.utterance_id,
                            utterance_revision: ctx.utterance_revision,
                            generation_id: ctx.generation_id,
                        }),
                    );
                    finish!();
                }
                Ok(ValidatedSuggestion::Text(text)) => {
                    first_visible_text_at = Some(Instant::now());
                    diagnostics.first_visible_token_at = Some(epoch_ms());
                    diagnostics.utterance_age_at_first_token_ms = Some(
                        request
                            .speech_ended_at
                            .elapsed()
                            .as_millis()
                            .min(u128::from(u64::MAX)) as u64,
                    );
                    diagnostics.validation_result = "valid".to_string();
                    diagnostics.final_text_length = text.chars().count();
                    if !text.is_empty()
                        && !self.publish_stream_event(
                            &app,
                            &ctx,
                            ResponseSuggestionEvent::Delta {
                                session_id: ctx.session_id,
                                turn_id: ctx.turn_id,
                                utterance_id: ctx.utterance_id,
                                utterance_revision: ctx.utterance_revision,
                                generation_id: ctx.generation_id,
                                text: text.clone(),
                            },
                        )
                    {
                        diagnostics.event_emitted = "discarded_stale".to_string();
                        diagnostics.terminal_state = "discarded_stale".to_string();
                        finish!();
                    }
                    if !text.is_empty() {
                        self.remember_suggestion_if_current(&ctx, &text);
                    }
                    diagnostics.event_emitted = if text.is_empty() {
                        "completed_empty".to_string()
                    } else {
                        "completed_with_text".to_string()
                    };
                    diagnostics.terminal_state = "completed".to_string();
                    self.publish_terminal_event(
                        &app,
                        &ctx,
                        &terminal_emitted,
                        TerminalState::Completed,
                        Some(ResponseSuggestionEvent::Completed {
                            session_id: ctx.session_id,
                            turn_id: ctx.turn_id,
                            utterance_id: ctx.utterance_id,
                            utterance_revision: ctx.utterance_revision,
                            generation_id: ctx.generation_id,
                            text,
                        }),
                    );
                    finish!();
                }
                Err(failure) if attempt == 0 => {
                    diagnostics.validation_result = failure.as_str().to_string();
                    tracing::info!(
                        session_id = ctx.session_id.value(),
                        generation_id = ctx.generation_id.value(),
                        utterance_id = ctx.utterance_id.value(),
                        validation_result = failure.as_str(),
                        context_leak_score = diagnostics.context_leak_score,
                        "invalid suggestion; retrying once"
                    );
                }
                Err(SuggestionValidationFailure::EchoOfQuestion) if candidate_is_full_echo => {
                    // A retry já foi usada e o modelo devolveu o mesmo eco integral de
                    // novo: não há resposta real para mostrar, mas também não há nada
                    // para vazar (o texto visível é vazio). Termina como conclusão
                    // vazia, não como erro — retry de novo só produziria o mesmo eco.
                    diagnostics.validation_result = SuggestionValidationFailure::EchoOfQuestion
                        .as_str()
                        .to_string();
                    diagnostics.event_emitted = "completed_empty".to_string();
                    diagnostics.terminal_state = "completed".to_string();
                    self.publish_terminal_event(
                        &app,
                        &ctx,
                        &terminal_emitted,
                        TerminalState::Completed,
                        Some(ResponseSuggestionEvent::Completed {
                            session_id: ctx.session_id,
                            turn_id: ctx.turn_id,
                            utterance_id: ctx.utterance_id,
                            utterance_revision: ctx.utterance_revision,
                            generation_id: ctx.generation_id,
                            text: String::new(),
                        }),
                    );
                    finish!();
                }
                Err(failure) => {
                    diagnostics.validation_result = failure.as_str().to_string();
                    diagnostics.event_emitted = "invalid".to_string();
                    diagnostics.terminal_state = "invalid".to_string();
                    self.publish_terminal_event(
                        &app,
                        &ctx,
                        &terminal_emitted,
                        TerminalState::Invalid,
                        Some(ResponseSuggestionEvent::Invalid {
                            session_id: ctx.session_id,
                            turn_id: ctx.turn_id,
                            utterance_id: ctx.utterance_id,
                            utterance_revision: ctx.utterance_revision,
                            generation_id: ctx.generation_id,
                            failure,
                        }),
                    );
                    finish!();
                }
            }
        }
    }

    fn remember_suggestion_if_current(&self, ctx: &GenerationContext, text: &str) {
        let mut state = self.session.lock().expect("response engine mutex poisoned");
        if state.session_id == ctx.session_id
            && !state.ending
            && state
                .generations
                .get(&ctx.utterance_id)
                .is_some_and(|handle| handle.context == *ctx)
        {
            state.last_suggestion = Some(text.to_string());
        }
    }

    #[cfg(any())]
    async fn run_generation_legacy<R: tauri::Runtime>(
        self: Arc<Self>,
        app: AppHandle<R>,
        ctx: GenerationContext,
        terminal_emitted: Arc<AtomicBool>,
        cancel_token: CancellationToken,
        trigger: GenerationTrigger,
        request: ResponseGenerationRequest,
    ) {
        let provider = self
            .provider
            .lock()
            .expect("response engine mutex poisoned")
            .clone();
        let model = self.current_config().model;

        let utterance_age_at_generation_start_ms = request
            .speech_ended_at
            .elapsed()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        let maximum_age_ms = self.current_config().maximum_automatic_generation_age_ms;
        if request.automatic && utterance_age_at_generation_start_ms > maximum_age_ms {
            tracing::info!(
                session_id = ctx.session_id.value(),
                generation_id = ctx.generation_id.value(),
                turn_id = ctx.turn_id.value(),
                utterance_id = ctx.utterance_id.value(),
                utterance_revision = ctx.utterance_revision,
                utterance_age_ms = utterance_age_at_generation_start_ms,
                maximum_age_ms,
                "stale_input"
            );
            self.clear_if_current(&ctx);
            return;
        }

        let built = self.context_builder.build(&request);

        tracing::info!(
            session_id = ctx.session_id.value(),
            turn_id = ctx.turn_id.value(),
            utterance_id = ctx.utterance_id.value(),
            generation_id = ctx.generation_id.value(),
            context_turn_count = built.context_turn_count,
            context_character_count = built.context_character_count,
            "context_built"
        );
        tracing::debug!(
            session_id = ctx.session_id.value(),
            generation_id = ctx.generation_id.value(),
            prompt_preview = %built.sanitized_preview,
            "context_built (sanitized prompt)"
        );

        tracing::info!(
            provider = provider.provider_name(),
            session_id = ctx.session_id.value(),
            turn_id = ctx.turn_id.value(),
            utterance_id = ctx.utterance_id.value(),
            utterance_revision = ctx.utterance_revision,
            generation_id = ctx.generation_id.value(),
            "starting response generation"
        );

        // O trace desta fala foi aberto no primeiro chunk de áudio e ligado ao
        // `UtteranceId` quando a timeline finalizou a utterance. Ligá-lo agora à geração é
        // o último elo: sem ele, "fim da fala → primeiro token visível" não fecharia,
        // porque o trecho de transcrição e o de geração viveriam em traces distintos.
        let telemetry = crate::telemetry::recorder();
        let trace = telemetry.trace_for_utterance(ctx.utterance_id);
        if let Some(trace) = trace {
            telemetry.link_generation(trace, ctx.generation_id.value());
            telemetry.mark(trace, crate::telemetry::Milestone::GenerationStarted);
            telemetry.record_attributes(
                trace,
                crate::telemetry::TraceAttributes {
                    response_provider: Some(provider.provider_name().to_string()),
                    response_model: Some(model.clone()),
                    context_turn_count: Some(built.context_turn_count),
                    context_character_count: Some(built.context_character_count),
                    ..Default::default()
                },
            );
        }

        let started_at = Instant::now();
        let mut diagnostics = GenerationDiagnostics {
            session_id: ctx.session_id,
            generation_id: ctx.generation_id,
            turn_id: ctx.turn_id,
            utterance_id: ctx.utterance_id,
            provider: provider.provider_name().to_string(),
            model,
            request_started: epoch_ms(),
            http_status: None,
            first_chunk_received: None,
            raw_prefix: String::new(),
            skip_detected: false,
            echo_suppressed_characters: 0,
            cancel_reason: None,
            latency_ms: 0,
            final_text_length: 0,
            event_emitted: String::new(),
            finalization_reason: trigger.finalization_reason.clone(),
            gap_ms_used: trigger.gap_ms_used,
            silence_detected_ms: trigger.silence_detected_ms,
            context_turn_count: built.context_turn_count,
            context_character_count: built.context_character_count,
            prompt_preview: built.sanitized_preview.clone(),
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
                    &ctx,
                    $diagnostics,
                    started_at,
                    utterance_finalized_at,
                    request_started_at,
                    first_http_chunk_at,
                    first_visible_text_at,
                )
            };
        }

        if !self.publish_stream_event(
            &app,
            &ctx,
            ResponseSuggestionEvent::Started {
                session_id: ctx.session_id,
                turn_id: ctx.turn_id,
                utterance_id: ctx.utterance_id,
                generation_id: ctx.generation_id,
            },
        ) {
            diagnostics.event_emitted = "discarded_stale".to_string();
            finish!(diagnostics);
            return;
        }

        let mut invalid_retry_available = true;

        'generation_attempt: loop {
            let stream_result = tokio::select! {
                        _ = cancel_token.cancelled() => {
                            diagnostics.cancel_reason = Some(CANCEL_REASON_NEW_UTTERANCE.to_string());
                            diagnostics.event_emitted = "cancelled".to_string();
                            self.publish_terminal_event(
                                &app,
                                &ctx,
                                &terminal_emitted,
                                TerminalState::Cancelled,
                                Some(ResponseSuggestionEvent::Cancelled {
                                    session_id: ctx.session_id,
                                    turn_id: ctx.turn_id,
            utterance_id: ctx.utterance_id,
                                    generation_id: ctx.generation_id,
                                }),
                            );
                            finish!(diagnostics);
                            return;
                        }
                result = provider.stream_reply(built.request.clone()) => result,
            };

            let mut stream = match stream_result {
                Ok((s, meta)) => {
                    diagnostics.http_status = Some(meta.http_status);
                    s
                }
                Err(e) => {
                    diagnostics.event_emitted = "error".to_string();
                    self.publish_terminal_event(
                        &app,
                        &ctx,
                        &terminal_emitted,
                        TerminalState::Error,
                        Some(ResponseSuggestionEvent::Error {
                            session_id: ctx.session_id,
                            turn_id: ctx.turn_id,
                            utterance_id: ctx.utterance_id,
                            generation_id: ctx.generation_id,
                            message: e.to_string(),
                        }),
                    );
                    finish!(diagnostics);
                    return;
                }
            };

            let mut detector = SkipDetector::new();
            // Segundo filtro, depois do `SkipDetector` e antes de qualquer `Delta`: o modelo às
            // vezes começa repetindo a fala em vez de respondê-la, e a sugestão saía sendo a
            // própria pergunta. Ver `echo_guard.rs` — não é detecção de pergunta, é comparação
            // com a fala conhecida que originou esta geração.
            let mut echo_guard = EchoGuard::new(&trigger.utterance_text);
            // Tudo que o `SkipDetector` liberou, tenha o guarda deixado passar ou não — a
            // diferença para `full_text` no fim é exatamente o que foi suprimido como eco.
            let mut fed_characters = 0usize;
            let mut full_text = String::new();

            loop {
                let next = tokio::select! {
                                _ = cancel_token.cancelled() => {
                                    diagnostics.cancel_reason = Some(CANCEL_REASON_NEW_UTTERANCE.to_string());
                                    diagnostics.event_emitted = "cancelled".to_string();
                                    self.publish_terminal_event(
                                        &app,
                                        &ctx,
                                        &terminal_emitted,
                                        TerminalState::Cancelled,
                                        Some(ResponseSuggestionEvent::Cancelled {
                                            session_id: ctx.session_id,
                                            turn_id: ctx.turn_id,
                utterance_id: ctx.utterance_id,
                                            generation_id: ctx.generation_id,
                                        }),
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
                                tracing::info!(
                                    session_id = ctx.session_id.value(),
                                    turn_id = ctx.turn_id.value(),
                                    generation_id = ctx.generation_id.value(),
                                    "skip_detected"
                                );
                                self.publish_terminal_event(
                                    &app,
                                    &ctx,
                                    &terminal_emitted,
                                    TerminalState::Skipped,
                                    Some(ResponseSuggestionEvent::Skipped {
                                        session_id: ctx.session_id,
                                        turn_id: ctx.turn_id,
                                        utterance_id: ctx.utterance_id,
                                        generation_id: ctx.generation_id,
                                    }),
                                );
                                finish!(diagnostics);
                                return;
                            }
                            SkipDecision::NotSkip { flush } => {
                                fed_characters += flush.chars().count();
                                let visible = echo_guard.push(&flush);
                                if !visible.is_empty() {
                                    if first_visible_text_at.is_none() {
                                        first_visible_text_at = Some(Instant::now());
                                    }
                                    full_text.push_str(&visible);
                                    if !self.publish_stream_event(
                                        &app,
                                        &ctx,
                                        ResponseSuggestionEvent::Delta {
                                            session_id: ctx.session_id,
                                            turn_id: ctx.turn_id,
                                            utterance_id: ctx.utterance_id,
                                            generation_id: ctx.generation_id,
                                            text: visible,
                                        },
                                    ) {
                                        // Sessão encerrada ou geração substituída no meio do
                                        // stream: para de consumir e não publica mais nada.
                                        diagnostics.event_emitted = "discarded_stale".to_string();
                                        finish!(diagnostics);
                                        return;
                                    }
                                }
                            }
                        }
                    }
                    Ok(ResponseChunk::Done) => break,
                    Err(e) => {
                        diagnostics.event_emitted = "error".to_string();
                        self.publish_terminal_event(
                            &app,
                            &ctx,
                            &terminal_emitted,
                            TerminalState::Error,
                            Some(ResponseSuggestionEvent::Error {
                                session_id: ctx.session_id,
                                turn_id: ctx.turn_id,
                                utterance_id: ctx.utterance_id,
                                generation_id: ctx.generation_id,
                                message: e.to_string(),
                            }),
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
                    tracing::info!(
                        session_id = ctx.session_id.value(),
                        turn_id = ctx.turn_id.value(),
                        generation_id = ctx.generation_id.value(),
                        "skip_detected"
                    );
                    self.publish_terminal_event(
                        &app,
                        &ctx,
                        &terminal_emitted,
                        TerminalState::Skipped,
                        Some(ResponseSuggestionEvent::Skipped {
                            session_id: ctx.session_id,
                            turn_id: ctx.turn_id,
                            utterance_id: ctx.utterance_id,
                            generation_id: ctx.generation_id,
                        }),
                    );
                }
                SkipDecision::NotSkip { flush } => {
                    // Fim do stream: o que o guarda ainda estiver segurando precisa sair agora
                    // (ou ser descartado agora), senão uma resposta curta sem pontuação final
                    // nunca chegaria à UI.
                    fed_characters += flush.chars().count();
                    let mut visible = echo_guard.push(&flush);
                    visible.push_str(&echo_guard.finish());
                    if !visible.is_empty() {
                        if first_visible_text_at.is_none() {
                            first_visible_text_at = Some(Instant::now());
                        }
                        full_text.push_str(&visible);
                        self.publish_stream_event(
                            &app,
                            &ctx,
                            ResponseSuggestionEvent::Delta {
                                session_id: ctx.session_id,
                                turn_id: ctx.turn_id,
                                utterance_id: ctx.utterance_id,
                                generation_id: ctx.generation_id,
                                text: visible,
                            },
                        );
                    }
                    if is_invalid_generated_response(&full_text) && invalid_retry_available {
                        invalid_retry_available = false;
                        diagnostics.raw_prefix.clear();
                        diagnostics.first_chunk_received = None;
                        diagnostics.event_emitted = "retry_invalid_generation".to_string();
                        let _ = self.publish_stream_event(
                            &app,
                            &ctx,
                            ResponseSuggestionEvent::Started {
                                session_id: ctx.session_id,
                                turn_id: ctx.turn_id,
                                utterance_id: ctx.utterance_id,
                                generation_id: ctx.generation_id,
                            },
                        );
                        continue 'generation_attempt;
                    }

                    diagnostics.event_emitted = if full_text.trim().is_empty() {
                        "completed_empty".to_string()
                    } else {
                        "completed_with_text".to_string()
                    };
                    self.publish_terminal_event(
                        &app,
                        &ctx,
                        &terminal_emitted,
                        TerminalState::Completed,
                        Some(ResponseSuggestionEvent::Completed {
                            session_id: ctx.session_id,
                            turn_id: ctx.turn_id,
                            utterance_id: ctx.utterance_id,
                            generation_id: ctx.generation_id,
                            text: full_text.clone(),
                        }),
                    );
                }
                SkipDecision::Pending => {
                    if is_invalid_generated_response(&full_text) && invalid_retry_available {
                        invalid_retry_available = false;
                        diagnostics.raw_prefix.clear();
                        diagnostics.first_chunk_received = None;
                        diagnostics.event_emitted = "retry_invalid_generation".to_string();
                        let _ = self.publish_stream_event(
                            &app,
                            &ctx,
                            ResponseSuggestionEvent::Started {
                                session_id: ctx.session_id,
                                turn_id: ctx.turn_id,
                                utterance_id: ctx.utterance_id,
                                generation_id: ctx.generation_id,
                            },
                        );
                        continue 'generation_attempt;
                    }

                    diagnostics.event_emitted = if full_text.trim().is_empty() {
                        "completed_empty".to_string()
                    } else {
                        "completed_with_text".to_string()
                    };
                    self.publish_terminal_event(
                        &app,
                        &ctx,
                        &terminal_emitted,
                        TerminalState::Completed,
                        Some(ResponseSuggestionEvent::Completed {
                            session_id: ctx.session_id,
                            turn_id: ctx.turn_id,
                            utterance_id: ctx.utterance_id,
                            generation_id: ctx.generation_id,
                            text: full_text.clone(),
                        }),
                    );
                }
            }

            diagnostics.final_text_length = full_text.chars().count();
            diagnostics.echo_suppressed_characters =
                fed_characters.saturating_sub(diagnostics.final_text_length);
            finish!(diagnostics);
            break;
        }
    }

    /// Fecha uma geração: registra `latency_ms`, computa as métricas de latência de ponta
    /// a ponta (relógio monotônico, não epoch), emite o evento de diagnóstico (só se a
    /// sessão dona ainda for a ativa) e libera o slot de `generations` se ainda for a
    /// geração corrente para o turno. Chamada em todo caminho de saída de
    /// `run_generation` (skip, erro, cancelamento, descarte por sessão ou conclusão), para
    /// que uma geração seguinte nunca veja um estado "fantasma" da anterior.
    #[allow(clippy::too_many_arguments)]
    fn finish_generation<R: tauri::Runtime>(
        &self,
        app: &AppHandle<R>,
        ctx: &GenerationContext,
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
        diagnostics.end_of_speech_to_first_visible_token_ms =
            diagnostics.utterance_age_at_first_token_ms;
        // Marcos gravados com os `Instant` capturados dentro do laço de streaming: pegar o
        // lock do recorder a cada chunk HTTP seria custo no caminho crítico sem ganho, já
        // que o valor medido é o mesmo.
        let telemetry = crate::telemetry::recorder();
        if let Some(trace) = telemetry.trace_for_generation(ctx.generation_id.value()) {
            use crate::telemetry::Milestone;
            if let Some(at) = first_http_chunk_at {
                telemetry.mark_at(trace, Milestone::FirstHttpChunk, at);
            }
            if let Some(at) = first_visible_text_at {
                telemetry.mark_at(trace, Milestone::FirstVisibleToken, at);
            }
            telemetry.mark(trace, Milestone::GenerationCompleted);
            // Fecha o trace em **todo** caminho de saída, inclusive skip, erro e
            // cancelamento — `finish_generation` é justamente o ponto por onde todos passam.
            // Não fechar aqui deixaria um trace vivo por fala descartada até a evicção.
            if let Some(snapshot) = telemetry.finish(trace) {
                tracing::debug!(
                    trace_id = %snapshot.trace_id,
                    latencies = ?snapshot.latencies,
                    "pipeline trace"
                );
            }
        }

        tracing::debug!(?diagnostics, "response generation diagnostics");
        let _ = self.publish_stream_event(
            app,
            ctx,
            ResponseSuggestionEvent::Diagnostics(Box::new(diagnostics)),
        );
        self.clear_if_current(ctx);
    }
}

/// Motivos de finalização que **nunca** disparam geração. Dois grupos, por razões
/// diferentes:
///
/// - `CaptureStopped`/`SessionEnded` são consequência de a sessão ou a captura estar
///   terminando, não de a outra pessoa ter parado de falar esperando uma resposta. Gerar
///   aqui era uma das rotas pelas quais a última pergunta de uma sessão aparecia
///   respondida já dentro da sessão seguinte (o frontend para a captura antes de encerrar
///   a sessão, então `CaptureStopped` chegava primeiro).
/// - `SpeakerChanged`/`SourceChanged`, numa utterance da outra pessoa, só podem significar
///   uma coisa: o microfone começou a produzir fala, ou seja, **o usuário tomou a palavra**.
///   Ele já está respondendo — uma sugestão agora chega tarde por definição. E o efeito era
///   ativamente destrutivo: a geração nova substituía, token a token, a sugestão que o
///   usuário estava lendo em voz alta naquele exato instante e, como a fala dele acabara de
///   entrar no contexto como `Você: ...`, o modelo com frequência devolvia a própria fala
///   dele de volta. O disparo legítimo é o silêncio (`InactivityTimeout`), já coberto pelo
///   timer dedicado da utterance.
fn triggers_generation(reason: UtteranceFinalizationReason) -> bool {
    match reason {
        UtteranceFinalizationReason::InactivityTimeout
        | UtteranceFinalizationReason::ManualFlush
        | UtteranceFinalizationReason::MaximumDuration => true,
        UtteranceFinalizationReason::SpeakerChanged
        | UtteranceFinalizationReason::SourceChanged
        | UtteranceFinalizationReason::CaptureStopped
        | UtteranceFinalizationReason::SessionEnded => false,
    }
}

fn log_conversation_event_received(engine: &ResponseEngine, event: &ConversationTimelineEvent) {
    let active_session_id = engine
        .session
        .lock()
        .expect("response engine mutex poisoned")
        .session_id;
    let (session_id, turn_id, utterance_id, revision, speaker, source) = match event {
        ConversationTimelineEvent::UtteranceStarted {
            utterance_id,
            turn_id,
            speaker,
            source,
            ..
        }
        | ConversationTimelineEvent::UtteranceUpdated {
            utterance_id,
            turn_id,
            speaker,
            source,
            ..
        } => (
            active_session_id,
            Some(*turn_id),
            Some(*utterance_id),
            None,
            Some(*speaker),
            Some(*source),
        ),
        ConversationTimelineEvent::UtteranceFinalized {
            turn_id,
            utterance,
            session_id,
            ..
        } => (
            *session_id,
            Some(*turn_id),
            Some(utterance.id),
            Some(utterance.revision),
            Some(utterance.speaker),
            Some(utterance.source),
        ),
        ConversationTimelineEvent::TurnStarted {
            turn_id,
            speaker,
            source,
            ..
        } => (
            active_session_id,
            Some(*turn_id),
            None,
            None,
            Some(*speaker),
            Some(*source),
        ),
        ConversationTimelineEvent::TurnUpdated { turn } => (
            active_session_id,
            Some(turn.id),
            None,
            None,
            Some(turn.speaker),
            Some(turn.source),
        ),
        ConversationTimelineEvent::TurnFinalized { turn, session_id } => (
            *session_id,
            Some(turn.id),
            None,
            None,
            Some(turn.speaker),
            Some(turn.source),
        ),
        ConversationTimelineEvent::SessionEnded { session_id }
        | ConversationTimelineEvent::SessionStarted { session_id } => {
            (*session_id, None, None, None, None, None)
        }
    };

    tracing::info!(
        event_type = event.event_type(),
        session_id = session_id.value(),
        turn_id = turn_id.map(TurnId::value),
        utterance_id = utterance_id.map(UtteranceId::value),
        revision,
        speaker = ?speaker,
        source = ?source,
        "response_engine_conversation_event_received"
    );
}

/// Único consumer do canal interno da `ConversationTimeline`. Ele nasce uma vez no setup
/// da aplicação e atravessa as trocas de sessão; sessão nova troca apenas o estado isolado
/// do motor, nunca o receiver. `broadcast` permite receivers adicionais de diagnóstico sem
/// fazer duas tasks disputarem um mesmo evento.
pub async fn run_response_engine_event_loop<R: tauri::Runtime>(
    app: AppHandle<R>,
    engine: Arc<ResponseEngine>,
    mut receiver: broadcast::Receiver<InternalConversationEventBatch>,
) {
    tracing::info!("response_engine_event_loop_started");
    loop {
        match receiver.recv().await {
            Ok(batch) => {
                for event in &batch.events {
                    log_conversation_event_received(&engine, event);
                }
                process_conversation_events_at(
                    &app,
                    engine.clone(),
                    &batch.events,
                    batch.published_at,
                );
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::error!(skipped, "response_engine_event_loop_error");
                engine.record_rejection(
                    GenerationRejectionReason::EngineNotReady,
                    None,
                    None,
                    format!("conversation event receiver lagged by {skipped} batches"),
                );
            }
            Err(broadcast::error::RecvError::Closed) => {
                tracing::info!("response_engine_event_loop_stopped");
                break;
            }
        }
    }
}

/// Inicia o worker e um supervisor separado. O supervisor observa `JoinError`, portanto um
/// panic da task nunca vira um `let _ = spawn(...)` silencioso.
pub fn start_response_engine_event_loop<R: tauri::Runtime>(
    app: AppHandle<R>,
    engine: Arc<ResponseEngine>,
    receiver: broadcast::Receiver<InternalConversationEventBatch>,
) {
    let worker = tauri::async_runtime::spawn(run_response_engine_event_loop(app, engine, receiver));
    let supervisor = tauri::async_runtime::spawn(async move {
        if let Err(error) = worker.await {
            tracing::error!(%error, "response_engine_event_loop_error");
            tracing::info!(
                reason = "worker_join_error",
                "response_engine_event_loop_stopped"
            );
        }
    });
    // Dropar um JoinHandle do Tokio apenas destaca a task; o supervisor continua vivo e
    // observa o worker até o encerramento ou panic.
    drop(supervisor);
}

/// Chamado a cada lote de eventos da Conversation Timeline. Mantém o histórico rolante
/// e dispara geração quando uma utterance de um turno elegível finaliza. Genérica sobre
/// `R: tauri::Runtime` pelo mesmo motivo de `trigger_generation`.
///
/// Todo evento carrega a sessão dona; nada aqui é aceito "no escuro". Turnos de uma sessão
/// que não é mais a ativa não entram no histórico, e utterances de uma sessão encerrada não
/// disparam geração.
#[cfg(test)]
pub fn process_conversation_events<R: tauri::Runtime>(
    app: &AppHandle<R>,
    engine: Arc<ResponseEngine>,
    events: &[ConversationTimelineEvent],
) {
    process_conversation_events_at(app, engine, events, Instant::now());
}

fn process_conversation_events_at<R: tauri::Runtime>(
    app: &AppHandle<R>,
    engine: Arc<ResponseEngine>,
    events: &[ConversationTimelineEvent],
    utterance_finalized_at: Instant,
) {
    let mut latest_turns: HashMap<TurnId, ConversationTurn> = HashMap::new();
    for event in events {
        match event {
            ConversationTimelineEvent::TurnUpdated { turn }
            | ConversationTimelineEvent::TurnFinalized { turn, .. } => {
                latest_turns.insert(turn.id, turn.clone());
            }
            _ => {}
        }
    }

    for event in events {
        let ConversationTimelineEvent::UtteranceFinalized {
            turn_id,
            utterance,
            finalization_reason,
            gap_ms_used,
            silence_detected_ms,
            session_id,
        } = event
        else {
            continue;
        };
        tracing::info!(
            session_id = session_id.value(),
            turn_id = turn_id.value(),
            utterance_id = utterance.id.value(),
            revision = utterance.revision,
            speaker = ?utterance.speaker,
            source = ?utterance.source,
            finalization_reason = finalization_reason.as_str(),
            "response_engine_trigger_considered"
        );
        if !triggers_generation(*finalization_reason) {
            tracing::info!(
                session_id = session_id.value(),
                turn_id = turn_id.value(),
                utterance_id = utterance.id.value(),
                revision = utterance.revision,
                finalization_reason = finalization_reason.as_str(),
                rejection_reason = "finalization_does_not_trigger_generation",
                "response_engine_trigger_rejected"
            );
        } else if let Some(turn) = latest_turns.get(turn_id) {
            if is_eligible_turn(turn) {
                tracing::info!(
                    session_id = session_id.value(),
                    turn_id = turn_id.value(),
                    utterance_id = utterance.id.value(),
                    revision = utterance.revision,
                    speaker = ?utterance.speaker,
                    source = ?utterance.source,
                    finalization_reason = finalization_reason.as_str(),
                    "response_engine_trigger_accepted"
                );
                let trigger = GenerationTrigger {
                    session_id: *session_id,
                    utterance_id: utterance.id,
                    utterance_revision: utterance.revision,
                    utterance_text: utterance.text.clone(),
                    utterance: utterance.clone(),
                    utterance_finalized_at,
                    speech_ended_at: utterance.speech_ended_at,
                    automatic: true,
                    finalization_reason: finalization_reason.as_str().to_string(),
                    gap_ms_used: *gap_ms_used,
                    silence_detected_ms: *silence_detected_ms,
                };
                engine
                    .clone()
                    .trigger_generation(app.clone(), turn.clone(), trigger);
            } else if turn.speaker != ConversationSpeaker::OtherPerson {
                engine.record_rejection(
                    GenerationRejectionReason::WrongSpeaker,
                    Some(*turn_id),
                    Some(utterance.id),
                    format!("turn speaker is {:?}, not OtherPerson", turn.speaker),
                );
            } else {
                engine.record_rejection(
                    GenerationRejectionReason::WrongSource,
                    Some(*turn_id),
                    Some(utterance.id),
                    format!("turn source is {:?}, not SystemOutput", turn.source),
                );
            }
        } else {
            engine.record_rejection(
                GenerationRejectionReason::EngineNotReady,
                Some(*turn_id),
                Some(utterance.id),
                "utterance_finalized batch did not contain TurnUpdated/TurnFinalized",
            );
        }
        // O trigger acima materializou o snapshot antes de esta utterance entrar
        // no historico. Assim a fala atual nunca se duplica no contexto.
        engine.push_history(*session_id, utterance.clone());
    }
}

#[cfg(test)]
#[path = "engine_critical_tests.rs"]
mod critical_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::segment::{AudioTimestamp, SegmentId};
    use crate::conversation::{
        ConversationUtterance, SessionId, UtteranceFinalizationReason, UtteranceId,
    };
    use crate::response_provider::provider::{ResponseStream, ResponseStreamMeta};
    use futures_util::stream;
    use std::collections::VecDeque;
    use std::time::Duration;
    use tauri::Listener;
    use tokio::sync::mpsc;

    #[test]
    fn punctuation_only_generation_is_invalid() {
        assert!(is_invalid_generated_response("?"));
        assert!(is_invalid_generated_response("."));
    }

    #[test]
    fn farewell_only_generation_is_invalid() {
        assert!(is_invalid_generated_response("Tchau!"));
        assert!(is_invalid_generated_response("tchau"));
    }

    #[test]
    fn empty_generation_is_invalid_but_technical_answer_is_valid() {
        assert!(is_invalid_generated_response(" \n"));
        assert!(!is_invalid_generated_response(
            "Eu separaria o domínio da persistência e mapearia EF só na infraestrutura."
        ));
    }

    // `ResponseEngine::from_config_path` reads/derives config from disk and picks a real
    // provider by kind — not useful for testing the orchestration logic in this file.
    // This constructor plugs an arbitrary fake `ResponseProvider` directly, bypassing
    // config I/O entirely.
    impl ResponseEngine {
        pub(super) fn for_test(provider: Arc<dyn ResponseProvider>) -> Arc<Self> {
            Arc::new(ResponseEngine {
                provider: Mutex::new(provider),
                context_builder: Arc::new(super::super::context::DefaultResponseContextBuilder),
                config: Mutex::new(ResponseProviderConfig::default()),
                config_path: PathBuf::from("unused-in-tests.json"),
                session: Mutex::new(SessionState::new(SessionId::new())),
                next_generation_id: AtomicU64::new(0),
                last_rejection: Mutex::new(None),
            })
        }

        /// Token raiz da sessão ativa — só os testes precisam olhar para ele diretamente,
        /// para provar que uma sessão nova nunca recebe um token já cancelado.
        fn session_token_for_test(&self) -> CancellationToken {
            self.session
                .lock()
                .expect("response engine mutex poisoned")
                .cancel
                .clone()
        }

        fn active_generation_count(&self) -> usize {
            self.session
                .lock()
                .expect("response engine mutex poisoned")
                .generations
                .len()
        }

        fn history_len_for_test(&self) -> usize {
            self.session
                .lock()
                .expect("response engine mutex poisoned")
                .history
                .len()
        }
    }

    // Builds a fresh stream on every `stream_reply` call (not a one-shot `take()`) so the
    // same provider instance can be reused across multiple generations within one test —
    // exactly like the real, config-driven providers, which are only rebuilt when the
    // response provider configuration changes, not once per generation.
    enum FakeBehavior {
        RepliesWith(String),
        RepliesInOrder(Mutex<VecDeque<String>>),
        /// Never yields anything and never ends — stays "in flight" until the caller
        /// cancels it (via a new trigger for the same turn) or the test drops it. Used to
        /// test cancellation/replacement of an active generation.
        Hangs,
        /// A requisição em si falha (nem chega a abrir stream).
        FailsRequest,
        /// Erro no meio do stream, depois de um delta.
        FailsMidStream(String),
        /// Stream dirigido pelo teste: cada `send` do canal vira um chunk. Permite manter
        /// uma geração "lenta" aberta e injetar deltas *depois* de a sessão ter sido
        /// encerrada, que é exatamente o cenário C da validação manual.
        Scripted(Mutex<Option<mpsc::UnboundedReceiver<ResponseChunk>>>),
    }

    struct FakeProvider {
        behavior: FakeBehavior,
        /// Todo prompt efetivamente enviado ao "provedor", em ordem. É sobre isto que os
        /// testes de isolamento provam que nenhum texto de uma sessão anterior entrou.
        requests: Mutex<Vec<ResponseRequest>>,
    }

    impl FakeProvider {
        fn new(behavior: FakeBehavior) -> Arc<Self> {
            Arc::new(FakeProvider {
                behavior,
                requests: Mutex::new(Vec::new()),
            })
        }

        fn with_text(text: &str) -> Arc<Self> {
            FakeProvider::new(FakeBehavior::RepliesWith(text.to_string()))
        }

        fn with_texts(texts: &[&str]) -> Arc<Self> {
            FakeProvider::new(FakeBehavior::RepliesInOrder(Mutex::new(
                texts.iter().map(|text| (*text).to_string()).collect(),
            )))
        }

        fn hanging() -> Arc<Self> {
            FakeProvider::new(FakeBehavior::Hangs)
        }

        fn scripted() -> (Arc<Self>, mpsc::UnboundedSender<ResponseChunk>) {
            let (tx, rx) = mpsc::unbounded_channel();
            (
                FakeProvider::new(FakeBehavior::Scripted(Mutex::new(Some(rx)))),
                tx,
            )
        }

        fn prompts(&self) -> Vec<String> {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .map(|request| {
                    request
                        .messages
                        .iter()
                        .map(|m| m.content.clone())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .collect()
        }

        fn request_count(&self) -> usize {
            self.requests.lock().unwrap().len()
        }
    }

    #[async_trait::async_trait]
    impl ResponseProvider for FakeProvider {
        fn id(&self) -> super::super::provider::ResponseProviderId {
            super::super::provider::ResponseProviderId::Misconfigured
        }

        fn capabilities(&self) -> super::super::provider::ResponseProviderCapabilities {
            super::super::provider::ResponseProviderCapabilities::none()
        }

        fn provider_name(&self) -> &'static str {
            "fake"
        }

        async fn stream_reply(
            &self,
            request: ResponseRequest,
        ) -> Result<(ResponseStream, ResponseStreamMeta), ResponseProviderError> {
            self.requests.lock().unwrap().push(request);
            let stream: ResponseStream = match &self.behavior {
                FakeBehavior::RepliesWith(text) => {
                    Box::pin(stream::iter(vec![Ok(ResponseChunk::Delta(text.clone()))]))
                }
                FakeBehavior::RepliesInOrder(texts) => {
                    let text = texts
                        .lock()
                        .unwrap()
                        .pop_front()
                        .expect("fake provider sequence exhausted");
                    Box::pin(stream::iter(vec![Ok(ResponseChunk::Delta(text))]))
                }
                FakeBehavior::Hangs => Box::pin(stream::pending()),
                FakeBehavior::FailsRequest => {
                    return Err(ResponseProviderError::Network("falha simulada".to_string()))
                }
                FakeBehavior::FailsMidStream(text) => Box::pin(stream::iter(vec![
                    Ok(ResponseChunk::Delta(text.clone())),
                    Err(ResponseProviderError::Network("stream quebrou".to_string())),
                ])),
                FakeBehavior::Scripted(slot) => {
                    let rx = slot
                        .lock()
                        .unwrap()
                        .take()
                        .expect("scripted provider only supports one generation per test");
                    Box::pin(stream::unfold(rx, |mut rx| async move {
                        rx.recv().await.map(|chunk| (Ok(chunk), rx))
                    }))
                }
            };
            Ok((stream, ResponseStreamMeta { http_status: 200 }))
        }
    }

    fn turn(id: u64, speaker: ConversationSpeaker, source: AudioSource) -> ConversationTurn {
        turn_with_text(id, speaker, source, "texto do turno")
    }

    fn turn_with_text(
        id: u64,
        speaker: ConversationSpeaker,
        source: AudioSource,
        text: &str,
    ) -> ConversationTurn {
        ConversationTurn {
            capture_stream_id: crate::audio::types::CaptureStreamId::UNASSIGNED,
            id: TurnId::from_raw(id),
            speaker,
            source,
            text: text.to_string(),
            raw_text: text.to_string(),
            utterances: Vec::new(),
            started_at: AudioTimestamp(0),
            ended_at: AudioTimestamp(1_000),
            finalized_at: None,
        }
    }

    fn remote_turn(id: u64, text: &str) -> ConversationTurn {
        turn_with_text(
            id,
            ConversationSpeaker::OtherPerson,
            AudioSource::SystemOutput,
            text,
        )
    }

    fn remote_utterance(id: u64, text: &str) -> ConversationUtterance {
        ConversationUtterance {
            capture_stream_id: crate::audio::types::CaptureStreamId::UNASSIGNED,
            id: UtteranceId::from_raw(id),
            speaker: ConversationSpeaker::OtherPerson,
            source: AudioSource::SystemOutput,
            text: text.to_string(),
            raw_text: text.to_string(),
            segments: vec![SegmentId::next()],
            received_sequence: id,
            started_at: AudioTimestamp(id * 1_000),
            ended_at: AudioTimestamp(id * 1_000 + 500),
            finalized_at: Some(AudioTimestamp(id * 1_000 + 500)),
            revision: 1,
            transcription_completed_at: Instant::now(),
            speech_ended_at: Instant::now(),
        }
    }

    /// Lote de eventos equivalente ao que a timeline emite quando uma utterance finaliza
    /// por silêncio, na sessão `session_id`.
    fn utterance_finalized_batch_in(
        turn: &ConversationTurn,
        session_id: SessionId,
    ) -> Vec<ConversationTimelineEvent> {
        utterance_finalized_batch_full(
            turn,
            session_id,
            &turn.text,
            UtteranceFinalizationReason::InactivityTimeout,
        )
    }

    fn utterance_finalized_batch_full(
        turn: &ConversationTurn,
        session_id: SessionId,
        utterance_text: &str,
        reason: UtteranceFinalizationReason,
    ) -> Vec<ConversationTimelineEvent> {
        let utterance = ConversationUtterance {
            capture_stream_id: crate::audio::types::CaptureStreamId::UNASSIGNED,
            id: UtteranceId::from_raw(turn.id.value()),
            speaker: turn.speaker,
            source: turn.source,
            text: utterance_text.to_string(),
            raw_text: utterance_text.to_string(),
            segments: vec![SegmentId::next()],
            received_sequence: turn.id.value(),
            started_at: turn.started_at,
            ended_at: turn.ended_at,
            finalized_at: Some(turn.ended_at),
            revision: 1,
            transcription_completed_at: Instant::now(),
            speech_ended_at: Instant::now(),
        };
        vec![
            ConversationTimelineEvent::UtteranceFinalized {
                turn_id: turn.id,
                utterance,
                finalization_reason: reason,
                gap_ms_used: 1_800,
                silence_detected_ms: Some(1_800),
                session_id,
            },
            ConversationTimelineEvent::TurnUpdated { turn: turn.clone() },
        ]
    }

    /// Como `utterance_finalized_batch_in`, mas com um `utterance_id` explícito e distinto
    /// do id derivado do turno. Necessário para simular corretamente uma **segunda**
    /// utterance finalizando dentro do mesmo turno ainda aberto: no pipeline real, cada
    /// utterance finalizada tem seu próprio `UtteranceId` (mesmo turn_id, id diferente) —
    /// usar `utterance_finalized_batch_in` duas vezes para o mesmo turno reenvia o
    /// *mesmo* `utterance_id` (derivado de `turn.id`), que representa reentrega/duplicata
    /// e é corretamente rejeitada como `AlreadyProcessed`, não uma segunda fala real.
    fn utterance_finalized_batch_with_utterance_id(
        turn: &ConversationTurn,
        utterance_id: u64,
        session_id: SessionId,
    ) -> Vec<ConversationTimelineEvent> {
        let utterance = ConversationUtterance {
            capture_stream_id: crate::audio::types::CaptureStreamId::UNASSIGNED,
            id: UtteranceId::from_raw(utterance_id),
            speaker: turn.speaker,
            source: turn.source,
            text: turn.text.clone(),
            raw_text: turn.text.clone(),
            segments: vec![SegmentId::next()],
            received_sequence: utterance_id,
            started_at: turn.started_at,
            ended_at: turn.ended_at,
            finalized_at: Some(turn.ended_at),
            revision: 1,
            transcription_completed_at: Instant::now(),
            speech_ended_at: Instant::now(),
        };
        vec![
            ConversationTimelineEvent::UtteranceFinalized {
                turn_id: turn.id,
                utterance,
                finalization_reason: UtteranceFinalizationReason::InactivityTimeout,
                gap_ms_used: 1_800,
                silence_detected_ms: Some(1_800),
                session_id,
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

    /// Rede de segurança contra um teste genuinamente travado — não modela nenhuma regra
    /// de negócio. Folgado de propósito: as gerações rodam no runtime global do Tauri,
    /// compartilhado por toda a suíte, então uma máquina carregada pode atrasar a entrega
    /// de um evento sem que nada esteja errado.
    const EVENT_WAIT_TIMEOUT: Duration = Duration::from_secs(30);

    /// Waits for the next captured event whose `type` matches `event_type`, ignoring any
    /// others in between (e.g. `delta` events before a `completed`).
    async fn wait_for_event_type(
        rx: &mut mpsc::UnboundedReceiver<serde_json::Value>,
        event_type: &str,
    ) -> serde_json::Value {
        tokio::time::timeout(EVENT_WAIT_TIMEOUT, async {
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
        tokio::time::timeout(EVENT_WAIT_TIMEOUT, async {
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

    /// Drena o canal por `window` e devolve todos os eventos recebidos. Usado para provar
    /// ausência (nenhum evento de sessão encerrada aparece), o que só pode ser verificado
    /// esperando uma janela de tempo real.
    async fn drain_for(
        rx: &mut mpsc::UnboundedReceiver<serde_json::Value>,
        window: Duration,
    ) -> Vec<serde_json::Value> {
        let mut events = Vec::new();
        let deadline = tokio::time::Instant::now() + window;
        while let Ok(Some(value)) = tokio::time::timeout_at(deadline, rx.recv()).await {
            events.push(value);
        }
        events
    }

    fn types_of(events: &[serde_json::Value]) -> Vec<String> {
        events
            .iter()
            .map(|e| e["type"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    /// Janela curta o suficiente para não tornar a suíte lenta, longa o suficiente para que
    /// uma task que fosse publicar algo (ela já está acordada e pronta) tivesse publicado.
    const QUIET_WINDOW: Duration = Duration::from_millis(150);

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

    #[test]
    fn only_silence_flush_and_maximum_duration_trigger_generation() {
        assert!(triggers_generation(
            UtteranceFinalizationReason::InactivityTimeout
        ));
        assert!(triggers_generation(
            UtteranceFinalizationReason::ManualFlush
        ));
        assert!(triggers_generation(
            UtteranceFinalizationReason::MaximumDuration
        ));
        assert!(!triggers_generation(
            UtteranceFinalizationReason::SessionEnded
        ));
        assert!(!triggers_generation(
            UtteranceFinalizationReason::CaptureStopped
        ));
    }

    /// Numa utterance da outra pessoa, esses dois motivos significam que o microfone
    /// começou a falar: o usuário tomou a palavra. Gerar aí substituía a sugestão que ele
    /// estava lendo em voz alta naquele momento.
    #[test]
    fn the_user_taking_the_floor_never_triggers_generation() {
        assert!(!triggers_generation(
            UtteranceFinalizationReason::SpeakerChanged
        ));
        assert!(!triggers_generation(
            UtteranceFinalizationReason::SourceChanged
        ));
    }

    #[tokio::test]
    async fn eligible_utterance_triggers_generation_and_user_speech_does_not() {
        let engine = ResponseEngine::for_test(FakeProvider::with_text("resposta sugerida"));
        let session = engine.active_session_id();
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let mut rx = capture_events(&handle);

        let remote = remote_turn(1, "e como você faria isso?");
        let user_turn = turn(2, ConversationSpeaker::User, AudioSource::Microphone);

        // Fala do usuário nunca deveria disparar uma sugestão de resposta — processada
        // primeiro para garantir que, se disparasse por engano, o evento chegaria antes do
        // da fala elegível.
        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_in(&user_turn, session),
        );
        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_in(&remote, session),
        );

        let started = wait_for_event_type(&mut rx, "started").await;
        assert_eq!(
            started["turn_id"], 1,
            "only the eligible (remote) turn generated a suggestion"
        );
        assert_eq!(started["session_id"], session.value());
        wait_for_event_type(&mut rx, "completed").await;
    }

    async fn assert_invalid_first_generation_is_retried_once(invalid: &str) {
        let provider = FakeProvider::with_texts(&[
            invalid,
            "Eu separaria o domínio da persistência e deixaria o Entity Framework na infraestrutura.",
        ]);
        let engine = ResponseEngine::for_test(provider.clone());
        let session = engine.active_session_id();
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let mut rx = capture_events(&handle);

        let remote = remote_turn(1, "Como você aplicaria DDD com Entity Framework?");
        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_in(&remote, session),
        );

        // Um único "started" para as duas tentativas: para o frontend, é a mesma
        // geração (mesmo generation_id) só sendo re-tentada internamente — anunciar
        // "started" de novo não teria efeito (o reducer ignora um started repetido para
        // o mesmo generation_id) e só adicionaria ruído.
        wait_for_event_type(&mut rx, "started").await;
        let completed = wait_for_event_type(&mut rx, "completed").await;

        assert_eq!(provider.request_count(), 2);
        assert_eq!(
            completed["text"],
            "Eu separaria o domínio da persistência e deixaria o Entity Framework na infraestrutura."
        );
    }

    #[tokio::test]
    async fn empty_generation_retries_once_with_same_context() {
        assert_invalid_first_generation_is_retried_once("").await;
    }

    #[tokio::test]
    async fn question_mark_only_generation_retries_once_with_same_context() {
        assert_invalid_first_generation_is_retried_once("?").await;
    }

    #[tokio::test]
    async fn farewell_only_generation_retries_once_with_same_context() {
        assert_invalid_first_generation_is_retried_once("Tchau!").await;
    }

    /// O bug: o usuário lê a sugestão em voz alta, o microfone capta, a utterance aberta
    /// da outra pessoa finaliza por `SpeakerChanged` e isso disparava uma geração nova —
    /// que ia substituindo, token a token, exatamente a resposta que ele estava lendo.
    #[tokio::test]
    async fn the_user_starting_to_speak_does_not_replace_the_suggestion_being_read() {
        let engine = ResponseEngine::for_test(FakeProvider::with_text("resposta sugerida"));
        let session = engine.active_session_id();
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let mut rx = capture_events(&handle);

        let remote = remote_turn(1, "e como você faria isso?");
        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_in(&remote, session),
        );
        let first = wait_for_event_type(&mut rx, "started").await;
        let first_generation_id = first["generation_id"].clone();
        wait_for_event_type(&mut rx, "completed").await;

        // O usuário toma a palavra: a utterance aberta da outra pessoa finaliza por troca
        // de speaker, não por silêncio.
        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_full(
                &remote,
                session,
                "e como você faria isso?",
                UtteranceFinalizationReason::SpeakerChanged,
            ),
        );

        let events = drain_for(&mut rx, QUIET_WINDOW).await;
        let new_generations: Vec<_> = events
            .iter()
            .filter(|e| e["type"] == "started" && e["generation_id"] != first_generation_id)
            .collect();
        assert!(
            new_generations.is_empty(),
            "o usuário falando não pode iniciar uma geração nova sobre a sugestão que ele \
             está lendo, mas iniciou: {new_generations:?}"
        );
    }

    #[tokio::test]
    async fn a_new_trigger_for_the_same_turn_cancels_the_generation_still_in_flight() {
        let engine = ResponseEngine::for_test(FakeProvider::hanging());
        let session = engine.active_session_id();
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let mut rx = capture_events(&handle);

        let remote = remote_turn(1, "primeira fala");
        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_in(&remote, session),
        );
        let first_started = wait_for_event_type(&mut rx, "started").await;
        let first_generation_id = first_started["generation_id"].clone();

        // A pessoa continua falando no mesmo turno: uma segunda utterance (id distinto,
        // mesmo turn_id) finaliza enquanto a primeira geração (que nunca produz nada, de
        // propósito) ainda está em andamento.
        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_with_utterance_id(
                &remote,
                remote.id.value() + 1000,
                session,
            ),
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
    async fn superseded_generation_emits_exactly_one_terminal_event() {
        let engine = ResponseEngine::for_test(FakeProvider::hanging());
        let session = engine.active_session_id();
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let mut rx = capture_events(&handle);

        let remote = remote_turn(1, "fala");
        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_in(&remote, session),
        );
        let first = wait_for_event_type(&mut rx, "started").await;
        let first_generation_id = first["generation_id"].clone();
        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_with_utterance_id(
                &remote,
                remote.id.value() + 1000,
                session,
            ),
        );

        let events = drain_for(&mut rx, QUIET_WINDOW).await;
        let terminals: Vec<_> = events
            .iter()
            .filter(|e| {
                e["generation_id"] == first_generation_id
                    && matches!(
                        e["type"].as_str(),
                        Some("cancelled") | Some("completed") | Some("skipped") | Some("error")
                    )
            })
            .collect();
        assert_eq!(
            terminals.len(),
            1,
            "a substituição publica exatamente um estado terminal para a geração antiga, \
             e a task cancelada não publica um segundo: {:?}",
            types_of(&events)
        );
    }

    #[tokio::test]
    async fn generation_state_is_released_after_completion_no_leftover_cancellation_later() {
        let engine = ResponseEngine::for_test(FakeProvider::with_text("ok"));
        let session = engine.active_session_id();
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let mut rx = capture_events(&handle);

        let remote = remote_turn(1, "fala");
        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_in(&remote, session),
        );
        wait_for_event_type(&mut rx, "diagnostics").await; // last event of the first generation
        assert_eq!(engine.active_generation_count(), 0);

        // A primeira geração já terminou naturalmente (não foi cancelada) — se o estado em
        // `generations` não tivesse sido liberado em `finish_generation`, este novo
        // disparo para o mesmo turno enxergaria uma entrada "fantasma" e emitiria um
        // `cancelled` espúrio antes do novo `started`.
        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_in(&remote, session),
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
    async fn state_is_released_after_an_error_terminal_state() {
        let engine = ResponseEngine::for_test(FakeProvider::new(FakeBehavior::FailsRequest));
        let session = engine.active_session_id();
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let mut rx = capture_events(&handle);

        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_in(&remote_turn(1, "fala"), session),
        );
        wait_for_event_type(&mut rx, "error").await;
        wait_for_event_type(&mut rx, "diagnostics").await;
        assert_eq!(engine.active_generation_count(), 0);
    }

    #[tokio::test]
    async fn mid_stream_error_ends_the_generation_exactly_once() {
        let engine = ResponseEngine::for_test(FakeProvider::new(FakeBehavior::FailsMidStream(
            "começo da resposta".to_string(),
        )));
        let session = engine.active_session_id();
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let mut rx = capture_events(&handle);

        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_in(&remote_turn(1, "fala"), session),
        );
        wait_for_event_type(&mut rx, "diagnostics").await;
        let all = drain_for(&mut rx, QUIET_WINDOW).await;
        assert!(
            !types_of(&all).contains(&"completed".to_string()),
            "um erro no meio do stream não pode ser seguido de um completed"
        );
        assert_eq!(engine.active_generation_count(), 0);
    }

    #[tokio::test]
    async fn skip_marker_ends_as_skipped_and_releases_state() {
        let engine = ResponseEngine::for_test(FakeProvider::with_text("[SKIP]"));
        let session = engine.active_session_id();
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let mut rx = capture_events(&handle);

        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_in(&remote_turn(1, "Perfeito."), session),
        );
        wait_for_event_type(&mut rx, "skipped").await;
        let diagnostics = wait_for_event_type(&mut rx, "diagnostics").await;
        assert_eq!(diagnostics["event_emitted"], "skipped");
        assert_eq!(diagnostics["skip_detected"], true);
        assert_eq!(engine.active_generation_count(), 0);

        let after = drain_for(&mut rx, QUIET_WINDOW).await;
        assert!(
            !types_of(&after).contains(&"delta".to_string()),
            "nada é exibido depois de um skip"
        );
    }

    #[tokio::test]
    async fn answer_text_ends_as_completed_with_text() {
        let engine = ResponseEngine::for_test(FakeProvider::with_text(
            "Já usei monolito quando o time era pequeno e o domínio ainda estava mudando.",
        ));
        let session = engine.active_session_id();
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let mut rx = capture_events(&handle);

        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_in(
                &remote_turn(
                    1,
                    "Me conta um caso real em que você optou por usar monolito.",
                ),
                session,
            ),
        );
        let completed = wait_for_event_type(&mut rx, "completed").await;
        assert!(completed["text"]
            .as_str()
            .unwrap()
            .contains("time era pequeno"));
        let diagnostics = wait_for_event_type(&mut rx, "diagnostics").await;
        assert_eq!(diagnostics["event_emitted"], "completed_with_text");
        assert_eq!(diagnostics["skip_detected"], false);
    }

    /// O modelo às vezes abre repetindo a pergunta antes de respondê-la. O eco não pode
    /// chegar à UI — a sugestão exibida tem que ser só a resposta.
    #[tokio::test]
    async fn an_echo_of_the_question_is_never_published_as_suggestion() {
        let engine = ResponseEngine::for_test(FakeProvider::with_text(
            "Em qual situação você escolheria usar micro-service? \
Acho que depende do tamanho do time.",
        ));
        let session = engine.active_session_id();
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let mut rx = capture_events(&handle);

        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_in(
                &remote_turn(1, "Em qual situação você escolheria usar monolitos?"),
                session,
            ),
        );

        let completed = wait_for_event_type(&mut rx, "completed").await;
        assert_eq!(
            completed["text"].as_str().unwrap(),
            "Acho que depende do tamanho do time."
        );
        let diagnostics = wait_for_event_type(&mut rx, "diagnostics").await;
        assert_eq!(diagnostics["event_emitted"], "completed_with_text");
        assert!(
            diagnostics["echo_suppressed_characters"].as_u64().unwrap() > 0,
            "o diagnóstico precisa registrar que houve eco suprimido"
        );
    }

    /// Resposta inteiramente ecoada: nada visível, e o estado final é uma conclusão vazia —
    /// nunca a pergunta de volta.
    #[tokio::test]
    async fn a_full_echo_leaves_no_visible_text_at_all() {
        let question = "Me conta um caso real em que você optou por usar monolito.";
        let engine = ResponseEngine::for_test(FakeProvider::with_text(question));
        let session = engine.active_session_id();
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let mut rx = capture_events(&handle);

        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_in(&remote_turn(1, question), session),
        );

        let completed = wait_for_event_type(&mut rx, "completed").await;
        assert_eq!(completed["text"].as_str().unwrap(), "");
        let diagnostics = wait_for_event_type(&mut rx, "diagnostics").await;
        assert_eq!(diagnostics["event_emitted"], "completed_empty");
    }

    /// O guarda não pode custar latência no caso normal: uma resposta que não começa
    /// repetindo a pergunta sai no primeiro delta, inteira.
    #[tokio::test]
    async fn an_ordinary_answer_is_not_held_back_by_the_echo_guard() {
        let engine = ResponseEngine::for_test(FakeProvider::with_text(
            "Já usei monolito quando o time era pequeno.",
        ));
        let session = engine.active_session_id();
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let mut rx = capture_events(&handle);

        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_in(
                &remote_turn(
                    1,
                    "Me conta um caso real em que você optou por usar monolito.",
                ),
                session,
            ),
        );

        let delta = wait_for_event_type(&mut rx, "delta").await;
        assert_eq!(
            delta["text"].as_str().unwrap(),
            "Já usei monolito quando o time era pequeno."
        );
        let diagnostics = wait_for_event_type(&mut rx, "diagnostics").await;
        assert_eq!(diagnostics["echo_suppressed_characters"], 0);
    }

    #[tokio::test]
    async fn prompt_classifies_the_current_utterance_not_the_whole_turn() {
        let provider = FakeProvider::with_text("resposta");
        let engine = ResponseEngine::for_test(provider.clone());
        let session = engine.active_session_id();
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let mut rx = capture_events(&handle);

        // O turno acumulou uma confirmação anterior e, agora, um pedido explícito. O que é
        // classificado tem que ser o pedido (a utterance que acabou de finalizar), não o
        // texto inteiro do turno.
        let turn = remote_turn(
            1,
            "Perfeito. Me conta um caso real em que você optou por usar monolito.",
        );
        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_full(
                &turn,
                session,
                "Me conta um caso real em que você optou por usar monolito.",
                UtteranceFinalizationReason::InactivityTimeout,
            ),
        );
        wait_for_event_type(&mut rx, "diagnostics").await;

        let prompt = provider.prompts().pop().expect("um prompt foi montado");
        let speech_start = prompt
            .find(super::super::context::CURRENT_SPEECH_HEADER)
            .unwrap();
        let instruction_start = prompt
            .find(super::super::context::INSTRUCTION_HEADER)
            .unwrap();
        let current_speech = &prompt[speech_start..instruction_start];
        assert!(current_speech.contains("Me conta um caso real"));
        let context_block =
            &prompt[prompt.find(super::super::context::CONTEXT_HEADER).unwrap()..speech_start];
        // O contexto não é mais formado a partir do texto acumulado do turno (ver
        // `docs/response-suggestion.md`, "Contrato atual de isolamento e publicação"): só
        // utterances finalizadas e separadas entram, e só quando a fala atual contém uma
        // referência explícita que precisa de antecedente. "Perfeito." nunca virou uma
        // utterance própria aqui, então não pode vazar para o contexto — isso seria
        // exatamente o vazamento de contexto obsoleto que a36744c fechou.
        assert!(
            !context_block.contains("Perfeito."),
            "texto de dentro do mesmo turno não pode vazar para o contexto sem ser sua própria utterance finalizada"
        );
        assert!(
            !context_block.contains("Me conta um caso real"),
            "a fala a classificar não pode aparecer também dentro do contexto"
        );
    }

    // --- Isolamento entre sessões ---

    #[tokio::test]
    async fn history_from_another_session_is_rejected() {
        let engine = ResponseEngine::for_test(FakeProvider::with_text("ok"));
        let stale_session = SessionId::new();

        engine.push_history(
            stale_session,
            remote_utterance(1, "fala da sessão anterior"),
        );
        assert_eq!(engine.history_len_for_test(), 0);

        let active = engine.active_session_id();
        engine.push_history(active, remote_utterance(2, "fala da sessão ativa"));
        assert_eq!(engine.history_len_for_test(), 1);
    }

    #[tokio::test]
    async fn history_snapshot_is_none_for_a_stale_session() {
        let engine = ResponseEngine::for_test(FakeProvider::with_text("ok"));
        let first = engine.active_session_id();
        engine.push_history(first, remote_utterance(1, "fala"));
        assert!(engine.history_snapshot(first).is_some());

        engine.end_session(first);
        engine.begin_session(SessionId::new());
        assert!(
            engine.history_snapshot(first).is_none(),
            "o histórico da sessão encerrada não é acessível nem por quem tem o id dela"
        );
        assert!(engine
            .history_snapshot(engine.active_session_id())
            .is_some());
        assert_eq!(engine.history_len_for_test(), 0);
    }

    #[tokio::test]
    async fn a_new_session_starts_with_an_empty_context() {
        let provider = FakeProvider::with_text("resposta");
        let engine = ResponseEngine::for_test(provider.clone());
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let mut rx = capture_events(&handle);

        // --- Sessão A ---
        let session_a = engine.active_session_id();
        let turn_a = remote_turn(1, "Em qual situação você escolheria usar monolitos?");
        process_conversation_events(
            &handle,
            engine.clone(),
            &[ConversationTimelineEvent::TurnFinalized {
                turn: turn_a.clone(),
                session_id: session_a,
            }],
        );
        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_in(&turn_a, session_a),
        );
        wait_for_event_type(&mut rx, "diagnostics").await;
        assert!(provider.prompts()[0].contains("monolitos"));

        // --- Fronteira ---
        engine.end_session(session_a);
        let session_b = SessionId::new();
        engine.begin_session(session_b);

        // --- Sessão B ---
        let turn_b = remote_turn(
            9,
            "Me conta um caso real em que você resolveu um problema de escalabilidade.",
        );
        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_in(&turn_b, session_b),
        );
        wait_for_event_type(&mut rx, "diagnostics").await;

        let prompt_b = provider.prompts().pop().expect("prompt da sessão B");
        assert!(prompt_b.contains("problema de escalabilidade"));
        assert!(
            !prompt_b.contains("monolito"),
            "nenhum texto da sessão A pode aparecer no prompt da sessão B:\n{prompt_b}"
        );
    }

    #[tokio::test]
    async fn a_trigger_from_a_previous_session_is_rejected() {
        let provider = FakeProvider::with_text("resposta");
        let engine = ResponseEngine::for_test(provider.clone());
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let mut rx = capture_events(&handle);

        let session_a = engine.active_session_id();
        engine.end_session(session_a);
        engine.begin_session(SessionId::new());

        // Evento atrasado da sessão A chegando depois da fronteira.
        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_in(&remote_turn(1, "pergunta da sessão A"), session_a),
        );

        let events = drain_for(&mut rx, QUIET_WINDOW).await;
        assert!(
            events.is_empty(),
            "nenhum evento pode ser emitido por um gatilho de sessão encerrada: {:?}",
            types_of(&events)
        );
        assert_eq!(
            provider.request_count(),
            0,
            "nem sequer um prompt é montado para uma sessão encerrada"
        );
    }

    #[tokio::test]
    async fn ending_a_session_suppresses_deltas_and_terminal_events_of_the_generation_in_flight() {
        let (provider, chunks) = FakeProvider::scripted();
        let engine = ResponseEngine::for_test(provider.clone());
        let session_a = engine.active_session_id();
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let mut rx = capture_events(&handle);

        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_in(&remote_turn(1, "pergunta lenta"), session_a),
        );
        wait_for_event_type(&mut rx, "started").await;
        // Texto não validado não é mais publicado incrementalmente (ver
        // `docs/response-suggestion.md`, "Contrato atual de isolamento e publicação"): não
        // há `delta` intermediário para esperar. Em vez disso, dá tempo para a task
        // consumir o chunk e provar que a geração está mesmo em andamento antes da
        // fronteira de sessão.
        chunks
            .send(ResponseChunk::Delta("primeira parte ".to_string()))
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            !chunks.is_closed(),
            "com a geração viva, o stream ainda está sendo lido"
        );

        // Usuário encerra a sessão com a geração ainda em andamento e abre outra na
        // sequência (cenário C da validação manual).
        engine.end_session(session_a);
        engine.begin_session(SessionId::new());

        // O provedor "atrasado" continua produzindo — nada disso pode chegar ao frontend.
        let _ = chunks.send(ResponseChunk::Delta("segunda parte".to_string()));
        let _ = chunks.send(ResponseChunk::Done);
        drop(chunks);

        let events = drain_for(&mut rx, QUIET_WINDOW).await;
        assert!(
            events.is_empty(),
            "depois da fronteira, a geração da sessão A não emite mais nada: {:?}",
            types_of(&events)
        );
        assert_eq!(engine.active_generation_count(), 0);
    }

    /// Cancelar não pode significar apenas "parar de publicar". O stream tem que ser
    /// **largado**: enquanto ele existir, a conexão HTTP com o provedor segue aberta e o
    /// modelo segue gerando — em provedor de nuvem isso é cota queimada em texto que
    /// ninguém vai ler, e em Ollama local é a GPU ocupada quando a próxima fala chegar.
    /// O sinal observável de que o stream foi descartado é o outro lado do canal fechar.
    #[tokio::test]
    async fn cancelling_drops_the_provider_stream_instead_of_draining_it() {
        let (provider, chunks) = FakeProvider::scripted();
        let engine = ResponseEngine::for_test(provider.clone());
        let session = engine.active_session_id();
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let mut rx = capture_events(&handle);

        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_in(&remote_turn(1, "pergunta longa"), session),
        );
        wait_for_event_type(&mut rx, "started").await;
        // Texto não validado não é mais publicado incrementalmente, então não há `delta`
        // intermediário para esperar aqui — só dar tempo para a task consumir o chunk.
        chunks
            .send(ResponseChunk::Delta("começando".to_string()))
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            !chunks.is_closed(),
            "com a geração viva, o stream ainda está sendo lido"
        );

        // `end_session` cancela e, deliberadamente, não publica evento terminal da sessão
        // que acabou (ver `is_publishable`) — então o que se observa aqui não é um evento,
        // é o stream do provedor sendo descartado.
        engine.end_session(session);

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !chunks.is_closed() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("o stream do provedor precisa ser largado ao cancelar");
        assert!(chunks
            .send(ResponseChunk::Delta("ninguém lê".to_string()))
            .is_err());
        assert_eq!(engine.active_generation_count(), 0);
    }

    #[tokio::test]
    async fn a_generation_of_the_previous_session_does_not_block_the_same_turn_in_the_new_session()
    {
        let engine = ResponseEngine::for_test(FakeProvider::hanging());
        let session_a = engine.active_session_id();
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let mut rx = capture_events(&handle);

        let same_turn = remote_turn(1, "fala");
        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_in(&same_turn, session_a),
        );
        wait_for_event_type(&mut rx, "started").await;

        engine.end_session(session_a);
        let session_b = SessionId::new();
        engine.begin_session(session_b);
        assert_eq!(
            engine.active_generation_count(),
            0,
            "o slot de geração do turno não sobrevive à fronteira"
        );

        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_in(&same_turn, session_b),
        );
        let started = wait_for_event_type(&mut rx, "started").await;
        assert_eq!(started["session_id"], session_b.value());
    }

    // --- Ciclo de vida da sessão ---

    #[tokio::test]
    async fn end_session_cancels_the_active_generation_token() {
        let engine = ResponseEngine::for_test(FakeProvider::hanging());
        let session = engine.active_session_id();
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let mut rx = capture_events(&handle);

        let root = engine.session_token_for_test();
        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_in(&remote_turn(1, "fala"), session),
        );
        wait_for_event_type(&mut rx, "started").await;
        assert_eq!(engine.active_generation_count(), 1);

        engine.end_session(session);
        assert!(root.is_cancelled());
        assert_eq!(engine.active_generation_count(), 0);
    }

    #[tokio::test]
    async fn a_new_session_never_inherits_a_cancelled_token() {
        let engine = ResponseEngine::for_test(FakeProvider::with_text("ok"));
        let session_a = engine.active_session_id();
        let root_a = engine.session_token_for_test();
        engine.end_session(session_a);
        assert!(root_a.is_cancelled());

        engine.begin_session(SessionId::new());
        let root_b = engine.session_token_for_test();
        assert!(
            !root_b.is_cancelled(),
            "a sessão nova tem um token raiz próprio, não o token já cancelado da anterior"
        );
    }

    #[tokio::test]
    async fn each_generation_token_is_a_child_of_the_session_token() {
        let engine = ResponseEngine::for_test(FakeProvider::hanging());
        let session = engine.active_session_id();
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let mut rx = capture_events(&handle);

        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_in(&remote_turn(1, "fala"), session),
        );
        wait_for_event_type(&mut rx, "started").await;
        let generation_token = {
            let state = engine.session.lock().unwrap();
            state
                .generations
                .values()
                .next()
                .expect("uma geração ativa")
                .cancel
                .clone()
        };
        assert!(!generation_token.is_cancelled());

        engine.session_token_for_test().cancel();
        assert!(
            generation_token.is_cancelled(),
            "cancelar a sessão cancela toda geração em voo de uma vez"
        );
    }

    #[tokio::test]
    async fn ending_a_session_twice_is_a_no_op() {
        let engine = ResponseEngine::for_test(FakeProvider::hanging());
        let session = engine.active_session_id();
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let mut rx = capture_events(&handle);

        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_in(&remote_turn(1, "fala"), session),
        );
        wait_for_event_type(&mut rx, "started").await;

        engine.end_session(session);
        engine.end_session(session);
        engine.end_session(SessionId::new()); // sessão que nunca foi ativa

        let events = drain_for(&mut rx, QUIET_WINDOW).await;
        assert!(
            events.is_empty(),
            "encerrar de novo não pode publicar nada: {:?}",
            types_of(&events)
        );
        assert_eq!(engine.active_generation_count(), 0);
    }

    #[tokio::test]
    async fn a_trigger_while_the_session_is_ending_is_rejected() {
        let provider = FakeProvider::with_text("resposta");
        let engine = ResponseEngine::for_test(provider.clone());
        let session = engine.active_session_id();
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let mut rx = capture_events(&handle);

        // Encerrada, mas sem `begin_session` ainda: é a janela entre parar a captura e
        // abrir a próxima sessão.
        engine.end_session(session);
        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_in(&remote_turn(1, "fala tardia"), session),
        );

        let events = drain_for(&mut rx, QUIET_WINDOW).await;
        assert!(events.is_empty(), "{:?}", types_of(&events));
        assert_eq!(provider.request_count(), 0);
    }

    #[tokio::test]
    async fn generations_for_different_turns_coexist() {
        let engine = ResponseEngine::for_test(FakeProvider::hanging());
        let session = engine.active_session_id();
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let mut rx = capture_events(&handle);

        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_in(&remote_turn(1, "primeira pergunta"), session),
        );
        wait_for_event_type(&mut rx, "started").await;
        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_in(&remote_turn(2, "segunda pergunta"), session),
        );
        wait_for_event_type(&mut rx, "started").await;

        assert_eq!(engine.active_generation_count(), 2);
        let events = drain_for(&mut rx, QUIET_WINDOW).await;
        assert!(
            !types_of(&events).contains(&"cancelled".to_string()),
            "turnos diferentes não se cancelam entre si"
        );
    }

    /// O roteiro A/B/C do requisito de validação, automatizado com um provedor falso: a
    /// parte que depende de um LLM real (a *qualidade* da decisão de responder) não é
    /// afirmada aqui; o que é afirmado é o comportamento de sessão, que é o que estava
    /// quebrado.
    #[tokio::test]
    async fn sessions_a_b_and_c_script() {
        let (scripted, chunks) = FakeProvider::scripted();
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();

        // --- Sessão A: uma pergunta gera resposta, uma confirmação vira skip, encerra sem
        // geração ativa.
        let answering = FakeProvider::with_text("Escolheria monolito com time pequeno.");
        let engine = ResponseEngine::for_test(answering.clone());
        let mut rx = capture_events(&handle);
        let session_a = engine.active_session_id();

        let question = remote_turn(1, "Em qual situação você escolheria usar monolitos?");
        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_in(&question, session_a),
        );
        wait_for_event_type(&mut rx, "completed").await;
        process_conversation_events(
            &handle,
            engine.clone(),
            &[ConversationTimelineEvent::TurnFinalized {
                turn: question.clone(),
                session_id: session_a,
            }],
        );

        *engine.provider.lock().unwrap() = FakeProvider::with_text("[SKIP]");
        let ack = remote_turn(2, "Perfeito.");
        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_in(&ack, session_a),
        );
        wait_for_event_type(&mut rx, "skipped").await;
        wait_for_event_type(&mut rx, "diagnostics").await;
        assert_eq!(engine.active_generation_count(), 0);

        engine.end_session(session_a);
        let session_b = SessionId::new();
        engine.begin_session(session_b);

        // --- Sessão B: começa vazia; a nova pergunta é respondida sem nenhum resquício de A.
        let answering_b = FakeProvider::with_text("Reduzi o custo por request com cache.");
        *engine.provider.lock().unwrap() = answering_b.clone();
        let boundary_events = drain_for(&mut rx, QUIET_WINDOW).await;
        assert!(
            boundary_events.is_empty(),
            "a fronteira não emite eventos de sugestão: {:?}",
            types_of(&boundary_events)
        );
        assert_eq!(engine.history_len_for_test(), 0);

        let question_b = remote_turn(
            3,
            "Me conta um caso real em que você resolveu um problema de escalabilidade.",
        );
        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_in(&question_b, session_b),
        );
        wait_for_event_type(&mut rx, "completed").await;
        let prompt_b = answering_b.prompts().pop().expect("prompt da sessão B");
        assert!(prompt_b.contains("problema de escalabilidade"));
        // Sentinelas de conteúdo *da sessão A* — "Perfeito." não serve mais como sentinela
        // porque aparece literalmente no `SYSTEM_PROMPT` fixo, como exemplo de calibração
        // da política de `[SKIP]` (ver `context.rs`); casaria sem nenhum vazamento real.
        assert!(
            !prompt_b.contains("monolito") && !prompt_b.contains("Em qual situação"),
            "o prompt da sessão B não pode conter nada da sessão A:\n{prompt_b}"
        );

        // --- Sessão C: geração lenta interrompida pelo encerramento; a sessão seguinte não
        // vê delta, completed, error nem skipped da anterior.
        engine.end_session(session_b);
        let session_c = SessionId::new();
        engine.begin_session(session_c);
        *engine.provider.lock().unwrap() = scripted.clone();

        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_in(&remote_turn(4, "pergunta longa"), session_c),
        );
        wait_for_event_type(&mut rx, "started").await;
        engine.end_session(session_c);
        engine.begin_session(SessionId::new());
        let _ = chunks.send(ResponseChunk::Delta("resposta atrasada".to_string()));
        let _ = chunks.send(ResponseChunk::Done);
        drop(chunks);

        let leftovers = drain_for(&mut rx, QUIET_WINDOW).await;
        assert!(
            leftovers.is_empty(),
            "nada da sessão C pode aparecer na sessão seguinte: {:?}",
            types_of(&leftovers)
        );
    }

    #[tokio::test]
    async fn end_of_speech_to_first_visible_token_ms_is_computed_from_the_trigger() {
        let engine = ResponseEngine::for_test(FakeProvider::with_text("resposta útil"));
        let session = engine.active_session_id();
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let mut rx = capture_events(&handle);

        let remote = remote_turn(1, "e como você resolveria isso?");
        process_conversation_events(
            &handle,
            engine.clone(),
            &utterance_finalized_batch_in(&remote, session),
        );

        let diagnostics = wait_for_event_type(&mut rx, "diagnostics").await;
        assert_eq!(diagnostics["session_id"], session.value());
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
        assert!(diagnostics["prompt_preview"]
            .as_str()
            .unwrap()
            .contains(super::super::context::CURRENT_SPEECH_HEADER));
        assert!(diagnostics["context_character_count"].is_u64());
    }
}
