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
  /** Sessão dona da geração. O backend já descarta eventos de sessões encerradas antes de
   * emitir (ver docs/response-suggestion.md); aqui o campo serve para diagnóstico. */
  session_id: number;
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
  finalization_reason?: string;
  gap_ms_used?: number;
  silence_detected_ms?: number | null;
  utterance_id?: number;
  context_turn_count?: number;
  context_character_count?: number;
  prompt_preview?: string;
  utterance_finalized_to_request_started_ms?: number | null;
  request_to_first_http_chunk_ms?: number | null;
  request_to_first_visible_token_ms?: number | null;
  end_of_speech_to_first_visible_token_ms?: number | null;
}

export type SuggestionStatus =
  | "preparing"
  | "streaming"
  | "completed_with_text"
  | "completed_empty"
  | "skipped"
  | "cancelled"
  | "error";

export interface SuggestionState {
  /** A fala que esta sugestão responde. É a chave do estado — ver o comentário do reducer. */
  utteranceId: number;
  turnId: number;
  generationId: number;
  status: SuggestionStatus;
  text: string;
  errorMessage?: string;
}

/**
 * Reduz um evento de `response://suggestion-event` sobre o estado de sugestões,
 * **indexado por `utterance_id`, não por `turn_id`**.
 *
 * Um `ConversationTurn` agrupa tudo que a outra pessoa falou enquanto manteve a palavra e
 * pode conter várias perguntas. Indexando por turno, a resposta à segunda pergunta
 * sobrescrevia a resposta à primeira: a tela tinha um slot só e o conteúdo era trocado no
 * lugar. A fala (`utterance`) é a unidade que de fato corresponde a uma sugestão — uma
 * pergunta, uma resposta — então cada uma tem seu próprio registro e nada é substituído.
 *
 * Eventos de uma geração já superada (`generation_id` diferente do registrado) são
 * descartados: o backend cancela a geração em andamento quando a mesma fala é estendida.
 *
 * `completed` é dividido em `completed_with_text`/`completed_empty` conforme o texto final:
 * uma resposta vazia e um skip são estados diferentes na origem e precisam continuar
 * distinguíveis na UI.
 */
export function applyResponseSuggestionEvent(
  current: Record<number, SuggestionState>,
  event: ResponseSuggestionEventRef,
): Record<number, SuggestionState> {
  if (event.utterance_id === undefined) {
    return current;
  }

  if (event.type === "started") {
    return {
      ...current,
      [event.utterance_id]: {
        utteranceId: event.utterance_id,
        turnId: event.turn_id,
        generationId: event.generation_id,
        status: "preparing",
        text: "",
      },
    };
  }

  const existing = current[event.utterance_id];
  if (!existing || existing.generationId !== event.generation_id) {
    return current;
  }

  switch (event.type) {
    case "delta":
      return {
        ...current,
        [event.utterance_id]: {
          ...existing,
          status: "streaming",
          text: existing.text + (event.text ?? ""),
        },
      };
    case "completed": {
      const text = event.text ?? existing.text;
      const status: SuggestionStatus = text.trim().length > 0 ? "completed_with_text" : "completed_empty";
      return { ...current, [event.utterance_id]: { ...existing, status, text } };
    }
    case "skipped":
      return { ...current, [event.utterance_id]: { ...existing, status: "skipped" } };
    case "cancelled":
      return { ...current, [event.utterance_id]: { ...existing, status: "cancelled" } };
    case "error":
      return {
        ...current,
        [event.utterance_id]: { ...existing, status: "error", errorMessage: event.message },
      };
    default:
      return current;
  }
}

export interface ResponseSuggestionDiagnostics {
  session_id: number;
  turn_id: number;
  utterance_id: number;
  generation_id: number;
  /** Prompt sanitizado (estrutura + trechos limitados) realmente enviado ao provedor.
   * Só aparece em modo de desenvolvedor — nunca contém credenciais. */
  prompt_preview: string;
  context_turn_count: number;
  context_character_count: number;
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
  finalization_reason: string;
  gap_ms_used: number;
  silence_detected_ms: number | null;
  utterance_finalized_to_request_started_ms: number | null;
  request_to_first_http_chunk_ms: number | null;
  request_to_first_visible_token_ms: number | null;
  end_of_speech_to_first_visible_token_ms: number | null;
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
      session_id: event.session_id,
      turn_id: event.turn_id,
      utterance_id: event.utterance_id ?? 0,
      generation_id: event.generation_id,
      prompt_preview: event.prompt_preview ?? "",
      context_turn_count: event.context_turn_count ?? 0,
      context_character_count: event.context_character_count ?? 0,
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
      finalization_reason: event.finalization_reason ?? "",
      gap_ms_used: event.gap_ms_used ?? 0,
      silence_detected_ms: event.silence_detected_ms ?? null,
      utterance_finalized_to_request_started_ms:
        event.utterance_finalized_to_request_started_ms ?? null,
      request_to_first_http_chunk_ms: event.request_to_first_http_chunk_ms ?? null,
      request_to_first_visible_token_ms: event.request_to_first_visible_token_ms ?? null,
      end_of_speech_to_first_visible_token_ms:
        event.end_of_speech_to_first_visible_token_ms ?? null,
    },
  };
}
