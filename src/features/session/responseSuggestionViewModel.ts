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
  generationId: number;
  status: SuggestionStatus;
  text: string;
  /**
   * Texto da última sugestão com conteúdo (`completed_with_text`) para este turno, mantido
   * enquanto a geração seguinte ainda não produziu nenhum delta visível. Sem isso, o evento
   * `started` da nova geração apagaria a sugestão anterior da tela instantaneamente — a
   * pessoa continuar/mudar de assunto não deveria fazer a resposta útil já mostrada
   * desaparecer antes que exista algo melhor para colocar no lugar (ver
   * `docs/response-suggestion.md`, "continuação da fala da outra pessoa").
   */
  previousText?: string;
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
 *
 * `started` não apaga o texto de uma geração anterior concluída — ele fica em
 * `previousText` (status `preparing`) até o primeiro `delta` com conteúdo real da nova
 * geração chegar, quando finalmente substitui o que estava na tela.
 */
export function applyResponseSuggestionEvent(
  current: Record<number, SuggestionState>,
  event: ResponseSuggestionEventRef,
): Record<number, SuggestionState> {
  if (event.type === "started") {
    const existing = current[event.turn_id];
    const previousText =
      existing?.status === "completed_with_text" ? existing.text : existing?.previousText;
    return {
      ...current,
      [event.turn_id]: {
        generationId: event.generation_id,
        status: "preparing",
        text: "",
        previousText,
      },
    };
  }

  const existing = current[event.turn_id];
  if (!existing || existing.generationId !== event.generation_id) {
    return current;
  }

  switch (event.type) {
    case "delta": {
      const text = existing.text + (event.text ?? "");
      // Primeiro delta com conteúdo real: a nova geração já tem algo para mostrar, então a
      // resposta anterior pode finalmente sair de cena.
      const previousText = text.length > 0 ? undefined : existing.previousText;
      return {
        ...current,
        [event.turn_id]: { ...existing, status: "streaming", text, previousText },
      };
    }
    case "completed": {
      const text = event.text ?? existing.text;
      const status: SuggestionStatus = text.trim().length > 0 ? "completed_with_text" : "completed_empty";
      // Se esta geração terminou sem conteúdo (skip/vazia/erro/cancelada mais adiante),
      // `previousText` continua disponível pra UI não regredir para "nenhuma sugestão".
      const previousText = status === "completed_with_text" ? undefined : existing.previousText;
      return { ...current, [event.turn_id]: { ...existing, status, text, previousText } };
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
