use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tracing::warn;

use crate::conversation::{SessionId, TurnId, UtteranceId};

use super::engine::GenerationId;
use super::validation::SuggestionValidationFailure;

pub const RESPONSE_SUGGESTION_EVENT: &str = "response://suggestion-event";

/// Toda variante publica carrega a identidade completa. Nenhum listener precisa inferir
/// revisao ou utterance a partir do turno.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseSuggestionEvent {
    Started {
        session_id: SessionId,
        turn_id: TurnId,
        utterance_id: UtteranceId,
        utterance_revision: u64,
        generation_id: GenerationId,
    },
    Delta {
        session_id: SessionId,
        turn_id: TurnId,
        utterance_id: UtteranceId,
        utterance_revision: u64,
        generation_id: GenerationId,
        text: String,
    },
    Completed {
        session_id: SessionId,
        turn_id: TurnId,
        utterance_id: UtteranceId,
        utterance_revision: u64,
        generation_id: GenerationId,
        text: String,
    },
    Skipped {
        session_id: SessionId,
        turn_id: TurnId,
        utterance_id: UtteranceId,
        utterance_revision: u64,
        generation_id: GenerationId,
    },
    Invalid {
        session_id: SessionId,
        turn_id: TurnId,
        utterance_id: UtteranceId,
        utterance_revision: u64,
        generation_id: GenerationId,
        failure: SuggestionValidationFailure,
    },
    Cancelled {
        session_id: SessionId,
        turn_id: TurnId,
        utterance_id: UtteranceId,
        utterance_revision: u64,
        generation_id: GenerationId,
    },
    Error {
        session_id: SessionId,
        turn_id: TurnId,
        utterance_id: UtteranceId,
        utterance_revision: u64,
        generation_id: GenerationId,
        message: String,
    },
    Diagnostics(Box<GenerationDiagnostics>),
}

#[derive(Debug, Clone, Serialize)]
pub struct GenerationDiagnostics {
    pub session_id: SessionId,
    pub generation_id: GenerationId,
    pub turn_id: TurnId,
    pub utterance_id: UtteranceId,
    pub utterance_revision: u64,
    pub provider: String,
    pub model: String,

    pub trigger_text: String,
    pub trigger_text_hash: String,
    pub context_utterance_ids: Vec<UtteranceId>,
    pub context_turn_count: usize,
    pub context_character_count: usize,
    pub prompt_preview: String,

    pub speech_ended_at: u64,
    pub transcription_completed_at: Option<u64>,
    pub utterance_finalized_at: u64,
    pub generation_triggered_at: u64,
    pub request_started: u64,
    pub first_visible_token_at: Option<u64>,
    pub completed_at: u64,
    pub utterance_age_at_generation_start_ms: u64,
    pub utterance_age_at_first_token_ms: Option<u64>,

    pub http_status: Option<u16>,
    pub first_chunk_received: Option<u64>,
    pub raw_prefix: String,
    pub skip_detected: bool,
    pub echo_suppressed_characters: usize,
    pub validation_result: String,
    pub retry_used: bool,
    pub context_leak_score: f32,
    pub cancel_reason: Option<String>,
    pub latency_ms: u64,
    pub final_text_length: usize,
    pub event_emitted: String,
    pub terminal_state: String,

    pub finalization_reason: String,
    pub gap_ms_used: u64,
    pub silence_detected_ms: Option<u64>,
    pub utterance_finalized_to_request_started_ms: Option<u64>,
    pub request_to_first_http_chunk_ms: Option<u64>,
    pub request_to_first_visible_token_ms: Option<u64>,
    pub end_of_speech_to_first_visible_token_ms: Option<u64>,
}

pub fn emit_response_suggestion_event<R: tauri::Runtime>(
    app: &AppHandle<R>,
    event: ResponseSuggestionEvent,
) {
    if let Err(error) = app.emit(RESPONSE_SUGGESTION_EVENT, event) {
        warn!(%error, "failed to emit response suggestion event");
    }
}
