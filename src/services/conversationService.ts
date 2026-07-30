import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { ConversationTimelineEvent, ConversationTimelineSnapshot } from "../types/conversation";
import type { ResponseSuggestionEventRef } from "../features/session/responseSuggestionViewModel";

export const CONVERSATION_TIMELINE_EVENT = "conversation://timeline-event";
export const RESPONSE_SUGGESTION_EVENT = "response://suggestion-event";

export function getConversationTimelineSnapshot(): Promise<ConversationTimelineSnapshot> {
  return invoke("conversation_timeline_snapshot_command");
}

export function flushConversationTurns(): Promise<void> {
  return invoke("conversation_flush_turns_command");
}

/** Raw transcript segments, before utterance/turn assembly — developer tools only. */
export function getConversationRawSegments(): Promise<unknown[]> {
  return invoke("conversation_raw_segments_command");
}

export function endConversationSession(): Promise<void> {
  return invoke("conversation_end_session_command");
}

/** "Regenerar" / Ctrl+Cmd+Shift+Enter — manually re-triggers a suggestion for a turn
 * that already has one. Rejected by the backend if the turn isn't eligible (not the
 * other person's, or already gone from the timeline). */
export function regenerateSuggestion(turnId: number): Promise<void> {
  return invoke("conversation_regenerate_suggestion_command", { turnId });
}

export function onConversationTimelineEvent(handler: (event: ConversationTimelineEvent) => void): Promise<UnlistenFn> {
  return listen<ConversationTimelineEvent>(CONVERSATION_TIMELINE_EVENT, (event) => handler(event.payload));
}

export function onResponseSuggestionEvent(handler: (event: ResponseSuggestionEventRef) => void): Promise<UnlistenFn> {
  return listen<ResponseSuggestionEventRef>(RESPONSE_SUGGESTION_EVENT, (event) => handler(event.payload));
}

/** Dev-only: lets the developer tools panel try different silence gaps without a
 * rebuild. See docs/response-suggestion.md. */
export function getUtteranceGapMs(): Promise<number> {
  return invoke("conversation_get_utterance_gap_ms_command");
}

export function setUtteranceGapMs(gapMs: number): Promise<void> {
  return invoke("conversation_set_utterance_gap_ms_command", { gapMs });
}
