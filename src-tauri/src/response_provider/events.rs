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
    Diagnostics(GenerationDiagnostics),
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
}

pub fn emit_response_suggestion_event(app: &AppHandle, event: ResponseSuggestionEvent) {
    if let Err(e) = app.emit(RESPONSE_SUGGESTION_EVENT, &event) {
        warn!(%e, "failed to emit response suggestion event to frontend");
    }
}
