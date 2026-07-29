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
}

pub fn emit_response_suggestion_event(app: &AppHandle, event: ResponseSuggestionEvent) {
    if let Err(e) = app.emit(RESPONSE_SUGGESTION_EVENT, &event) {
        warn!(%e, "failed to emit response suggestion event to frontend");
    }
}
