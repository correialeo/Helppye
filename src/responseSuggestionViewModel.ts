export type ResponseSuggestionEventType =
  | "started"
  | "delta"
  | "completed"
  | "skipped"
  | "cancelled"
  | "error"
  | "diagnostics";

export interface ResponseSuggestionEventRef {
  type: ResponseSuggestionEventType;
  turn_id: number;
  generation_id: number;
  text?: string;
  message?: string;
  // Presentes apenas em eventos "diagnostics" — ver GenerationDiagnostics no backend
  // (src-tauri/src/response_provider/events.rs).
  provider?: string;
  model?: string;
  request_started?: number;
  http_status?: number | null;
  first_chunk_received?: number | null;
  raw_prefix?: string;
  skip_detected?: boolean;
  cancel_reason?: string | null;
  latency_ms?: number;
  final_text_length?: number;
  event_emitted?: string;
}

export type SuggestionStatus =
  | "streaming"
  | "completed_with_text"
  | "completed_empty"
  | "skipped"
  | "cancelled"
  | "error";

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
 *
 * `completed` é dividido em `completed_with_text`/`completed_empty` conforme o texto final:
 * antes disso, uma resposta vazia e um skip pareciam estados diferentes na origem mas nada
 * garantia que a UI os distinguisse — agora ambos carregam um status próprio.
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
    case "completed": {
      const text = event.text ?? existing.text;
      const status: SuggestionStatus = text.trim().length > 0 ? "completed_with_text" : "completed_empty";
      return { ...current, [event.turn_id]: { ...existing, status, text } };
    }
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

export interface ResponseSuggestionDiagnostics {
  turn_id: number;
  generation_id: number;
  provider: string;
  model: string;
  request_started: number;
  http_status: number | null;
  first_chunk_received: number | null;
  raw_prefix: string;
  skip_detected: boolean;
  cancel_reason: string | null;
  latency_ms: number;
  final_text_length: number;
  event_emitted: string;
}

/**
 * Reduz eventos "diagnostics" isoladamente do estado de sugestão visível ao usuário —
 * dados de depuração em modo dev, mantidos por turno (última geração).
 */
export function applyResponseSuggestionDiagnostics(
  current: Record<number, ResponseSuggestionDiagnostics>,
  event: ResponseSuggestionEventRef,
): Record<number, ResponseSuggestionDiagnostics> {
  if (event.type !== "diagnostics") {
    return current;
  }

  return {
    ...current,
    [event.turn_id]: {
      turn_id: event.turn_id,
      generation_id: event.generation_id,
      provider: event.provider ?? "",
      model: event.model ?? "",
      request_started: event.request_started ?? 0,
      http_status: event.http_status ?? null,
      first_chunk_received: event.first_chunk_received ?? null,
      raw_prefix: event.raw_prefix ?? "",
      skip_detected: event.skip_detected ?? false,
      cancel_reason: event.cancel_reason ?? null,
      latency_ms: event.latency_ms ?? 0,
      final_text_length: event.final_text_length ?? 0,
      event_emitted: event.event_emitted ?? "",
    },
  };
}
