//! Canal de eventos de sugestão de resposta em streaming, emitido ao frontend. Substitui
//! `question://detection-event`: em vez de estados discretos de detecção, o frontend
//! recebe deltas de texto conforme o provedor de LLM gera a resposta.

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tracing::warn;

use crate::conversation::TurnId;

pub const RESPONSE_SUGGESTION_EVENT: &str = "response://suggestion-event";

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseSuggestionEvent {
    Started {
        turn_id: TurnId,
        generation_id: u64,
    },
    Delta {
        turn_id: TurnId,
        generation_id: u64,
        text: String,
    },
    Completed {
        turn_id: TurnId,
        generation_id: u64,
        text: String,
    },
    Skipped {
        turn_id: TurnId,
        generation_id: u64,
    },
    Cancelled {
        turn_id: TurnId,
        generation_id: u64,
    },
    Error {
        turn_id: TurnId,
        generation_id: u64,
        message: String,
    },
    // `GenerationDiagnostics` carries a couple dozen fields (latency breakdown, trigger
    // context) — boxed so this one variant doesn't force every other variant of this enum
    // (e.g. `Started { turn_id, generation_id }`) to reserve that much stack space too.
    Diagnostics(Box<GenerationDiagnostics>),
}

/// Registro de diagnóstico de uma geração completa, emitido sempre — independente de o
/// resultado ser skip, erro, cancelamento ou conclusão — para permitir distinguir em modo
/// dev os cinco estados finais possíveis (`event_emitted`): `skipped`, `error`,
/// `cancelled`, `completed_empty`, `completed_with_text`. Sem isso, um skip e uma resposta
/// vazia eram indistinguíveis a partir da UI.
#[derive(Debug, Clone, Serialize)]
pub struct GenerationDiagnostics {
    pub generation_id: u64,
    pub turn_id: TurnId,
    pub provider: String,
    pub model: String,
    /// Epoch ms de quando a requisição ao provedor começou.
    pub request_started: u64,
    /// Código HTTP da resposta, quando a requisição chegou a obter uma (ausente se a
    /// conexão falhou antes disso).
    pub http_status: Option<u16>,
    /// Epoch ms do primeiro chunk recebido do provedor, se algum chegou.
    pub first_chunk_received: Option<u64>,
    /// Primeiros caracteres brutos recebidos do provedor, antes de qualquer filtragem do
    /// `SkipDetector` — permite ver se o modelo de fato respondeu `[SKIP]`.
    pub raw_prefix: String,
    pub skip_detected: bool,
    /// Motivo do cancelamento, quando aplicável (hoje só existe uma causa: uma nova
    /// utterance no mesmo turno substituiu esta geração).
    pub cancel_reason: Option<String>,
    pub latency_ms: u64,
    pub final_text_length: usize,
    pub event_emitted: String,

    // --- Contexto do gatilho (por que esta geração começou) ---
    /// `UtteranceFinalizationReason` da utterance que disparou esta geração, como string
    /// (`inactivity_timeout`, `speaker_changed`, `source_changed`, ...) — ver
    /// `conversation::UtteranceFinalizationReason`.
    pub finalization_reason: String,
    /// `same_speaker_utterance_gap_ms` vigente no momento da finalização.
    pub gap_ms_used: u64,
    /// Silêncio efetivamente observado antes da finalização, quando mensurável — ver
    /// `ConversationTimelineEvent::UtteranceFinalized`.
    pub silence_detected_ms: Option<u64>,

    // --- Latência de ponta a ponta, medida com relógio monotônico (`Instant`), não
    // epoch — ver `docs/response-suggestion.md`. `None` quando o marco correspondente
    // nunca ocorreu (ex.: geração cancelada antes do primeiro chunk).
    /// Da utterance finalizada até a chamada ao provedor começar. Meta de engenharia:
    /// < 100 ms (ver `CLAUDE.md`) — é a métrica que mede diretamente o atraso de disparo
    /// que este trabalho corrigiu.
    pub utterance_finalized_to_request_started_ms: Option<u64>,
    /// Da chamada ao provedor até o primeiro chunk HTTP bruto (pode ser metadado vazio,
    /// não necessariamente texto visível).
    pub request_to_first_http_chunk_ms: Option<u64>,
    /// Da chamada ao provedor até o primeiro texto realmente exibido ao usuário (depois
    /// do `SkipDetector` liberar conteúdo) — a métrica de UX real de "tempo até a
    /// resposta começar a aparecer".
    pub request_to_first_visible_token_ms: Option<u64>,
    /// Métrica principal: da utterance finalizada até o primeiro texto visível. Soma o
    /// atraso de disparo (agora corrigido) com a latência do provedor.
    pub end_of_speech_to_first_visible_token_ms: Option<u64>,
}

/// Genérica sobre `R: tauri::Runtime` (em vez de `AppHandle` = `AppHandle<Wry>` fixo) só
/// para permitir testar `engine.rs` com `tauri::test::mock_app` (que produz um
/// `AppHandle<MockRuntime>`) sem precisar de uma janela/webview real. Em produção,
/// `AppHandle<Wry>` satisfaz o bound normalmente — nenhuma mudança de comportamento.
pub fn emit_response_suggestion_event<R: tauri::Runtime>(
    app: &AppHandle<R>,
    event: ResponseSuggestionEvent,
) {
    if let Err(e) = app.emit(RESPONSE_SUGGESTION_EVENT, &event) {
        warn!(%e, "failed to emit response suggestion event to frontend");
    }
}
