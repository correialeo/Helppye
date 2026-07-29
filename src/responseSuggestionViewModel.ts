export type ResponseSuggestionEventType =
  | "started"
  | "delta"
  | "completed"
  | "skipped"
  | "cancelled"
  | "error";

export interface ResponseSuggestionEventRef {
  type: ResponseSuggestionEventType;
  turn_id: number;
  generation_id: number;
  text?: string;
  message?: string;
}

export type SuggestionStatus = "streaming" | "completed" | "skipped" | "cancelled" | "error";

export interface SuggestionState {
  generationId: number;
  status: SuggestionStatus;
  text: string;
  errorMessage?: string;
}

/**
 * Reduz um evento de `response://suggestion-event` sobre o estado de sugestões por turno.
 * Descarta eventos de uma geração já superada (identificada por `generation_id`), já que o
 * backend pode cancelar uma geração em andamento e iniciar outra para o mesmo turno.
 */
export function applyResponseSuggestionEvent(
  current: Record<number, SuggestionState>,
  event: ResponseSuggestionEventRef,
): Record<number, SuggestionState> {
  if (event.type === "started") {
    return {
      ...current,
      [event.turn_id]: { generationId: event.generation_id, status: "streaming", text: "" },
    };
  }

  const existing = current[event.turn_id];
  if (!existing || existing.generationId !== event.generation_id) {
    return current;
  }

  switch (event.type) {
    case "delta":
      return {
        ...current,
        [event.turn_id]: { ...existing, text: existing.text + (event.text ?? "") },
      };
    case "completed":
      return {
        ...current,
        [event.turn_id]: { ...existing, status: "completed", text: event.text ?? existing.text },
      };
    case "skipped":
      return { ...current, [event.turn_id]: { ...existing, status: "skipped" } };
    case "cancelled":
      return { ...current, [event.turn_id]: { ...existing, status: "cancelled" } };
    case "error":
      return {
        ...current,
        [event.turn_id]: { ...existing, status: "error", errorMessage: event.message },
      };
    default:
      return current;
  }
}
