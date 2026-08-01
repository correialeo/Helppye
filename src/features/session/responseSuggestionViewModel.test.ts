import {
  applyResponseSuggestionDiagnostics,
  applyResponseSuggestionEvent,
  ResponseSuggestionDiagnostics,
  SuggestionState,
} from "./responseSuggestionViewModel";

function assert(condition: boolean, message: string): void {
  if (!condition) {
    throw new Error(`assertion failed: ${message}`);
  }
}

function run(name: string, fn: () => void): void {
  fn();
  console.log(`ok: ${name}`);
}

run("started creates an entry keyed by utterance, not by turn", () => {
  const state = applyResponseSuggestionEvent(
    {},
    { type: "started", session_id: 1, turn_id: 7, utterance_id: 42, utterance_revision: 1, generation_id: 1 },
  );
  assert(state[42]!.utteranceId === 42, "keyed by utterance_id");
  assert(state[42]!.turnId === 7, "turn recorded for the regenerate command");
  assert(state[7] === undefined, "nothing is keyed by turn_id");
  assert(state[42]!.status === "preparing", "status is preparing, not streaming, before any delta");
  assert(state[42]!.text === "", "text starts empty");
});

run("a second question in the same turn never overwrites the first answer", () => {
  let state: Record<number, SuggestionState> = {};
  state = applyResponseSuggestionEvent(state, {
    type: "started", session_id: 1, turn_id: 7, utterance_id: 1, utterance_revision: 1, generation_id: 1,
  });
  state = applyResponseSuggestionEvent(state, {
    type: "completed", session_id: 1, turn_id: 7, utterance_id: 1, utterance_revision: 1, generation_id: 1,
    text: "resposta à primeira pergunta",
  });
  // Mesmo turno (a outra pessoa continua com a palavra), fala nova.
  state = applyResponseSuggestionEvent(state, {
    type: "started", session_id: 1, turn_id: 7, utterance_id: 2, utterance_revision: 1, generation_id: 2,
  });
  state = applyResponseSuggestionEvent(state, {
    type: "completed", session_id: 1, turn_id: 7, utterance_id: 2, utterance_revision: 1, generation_id: 2,
    text: "resposta à segunda pergunta",
  });

  assert(state[1]!.text === "resposta à primeira pergunta", "first answer untouched");
  assert(state[2]!.text === "resposta à segunda pergunta", "second answer stored alongside it");
  assert(Object.keys(state).length === 2, "both answers coexist");
});

run("an event without utterance_id is ignored instead of corrupting state", () => {
  const before: Record<number, SuggestionState> = {
    1: { utteranceId: 1, turnId: 7, generationId: 1, sessionId: 1, utteranceRevision: 1, status: "completed_with_text", text: "resposta" },
  };
  const after = applyResponseSuggestionEvent(before, {
    type: "completed", session_id: 1, turn_id: 7, generation_id: 1, text: "sem utterance",
  });
  assert(after === before, "state returned unchanged");
});

run("delta appends text for the current generation", () => {
  let state: Record<number, SuggestionState> = {};
  state = applyResponseSuggestionEvent(state, {
    type: "started", session_id: 1, turn_id: 1, utterance_id: 1, utterance_revision: 1, generation_id: 1,
  });
  state = applyResponseSuggestionEvent(state, {
    type: "delta", session_id: 1, turn_id: 1, utterance_id: 1, utterance_revision: 1, generation_id: 1, text: "Olá",
  });
  state = applyResponseSuggestionEvent(state, {
    type: "delta", session_id: 1, turn_id: 1, utterance_id: 1, utterance_revision: 1, generation_id: 1, text: ", tudo bem?",
  });
  assert(state[1]!.status === "streaming", "status streaming after the first delta");
  assert(state[1]!.text === "Olá, tudo bem?", "text accumulated across deltas");
});

run("a new generation for the same utterance restarts its text", () => {
  let state: Record<number, SuggestionState> = {};
  state = applyResponseSuggestionEvent(state, {
    type: "started", session_id: 1, turn_id: 1, utterance_id: 1, utterance_revision: 1, generation_id: 1,
  });
  state = applyResponseSuggestionEvent(state, {
    type: "completed", session_id: 1, turn_id: 1, utterance_id: 1, utterance_revision: 1, generation_id: 1, text: "antiga",
  });
  state = applyResponseSuggestionEvent(state, {
    type: "started", session_id: 1, turn_id: 1, utterance_id: 1, utterance_revision: 1, generation_id: 2,
  });
  assert(state[1]!.generationId === 2, "generation id updated");
  assert(state[1]!.text === "", "text reset for the regenerated answer");
});

run("stale delta from a superseded generation is ignored", () => {
  let state: Record<number, SuggestionState> = {};
  state = applyResponseSuggestionEvent(state, {
    type: "started", session_id: 1, turn_id: 1, utterance_id: 1, utterance_revision: 1, generation_id: 1,
  });
  state = applyResponseSuggestionEvent(state, {
    type: "started", session_id: 1, turn_id: 1, utterance_id: 1, utterance_revision: 1, generation_id: 2,
  });
  state = applyResponseSuggestionEvent(state, {
    type: "delta", session_id: 1, turn_id: 1, utterance_id: 1, utterance_revision: 1, generation_id: 1, text: "texto antigo",
  });
  assert(state[1]!.generationId === 2, "still on the newer generation");
  assert(state[1]!.text === "", "stale delta had no effect");
});

run("completed with text replaces accumulated text and is completed_with_text", () => {
  let state: Record<number, SuggestionState> = {};
  state = applyResponseSuggestionEvent(state, {
    type: "started", session_id: 1, turn_id: 1, utterance_id: 1, utterance_revision: 1, generation_id: 1,
  });
  state = applyResponseSuggestionEvent(state, {
    type: "delta", session_id: 1, turn_id: 1, utterance_id: 1, utterance_revision: 1, generation_id: 1, text: "parcial",
  });
  state = applyResponseSuggestionEvent(state, {
    type: "completed", session_id: 1, turn_id: 1, utterance_id: 1, utterance_revision: 1, generation_id: 1,
    text: "texto final completo",
  });
  assert(state[1]!.status === "completed_with_text", "status completed_with_text");
  assert(state[1]!.text === "texto final completo", "text is the final text");
});

run("completed with empty text is a distinct completed_empty status, not skipped", () => {
  let state: Record<number, SuggestionState> = {};
  state = applyResponseSuggestionEvent(state, {
    type: "started", session_id: 1, turn_id: 1, utterance_id: 1, utterance_revision: 1, generation_id: 1,
  });
  state = applyResponseSuggestionEvent(state, {
    type: "completed", session_id: 1, turn_id: 1, utterance_id: 1, utterance_revision: 1, generation_id: 1, text: "",
  });
  assert(state[1]!.status === "completed_empty", "status completed_empty");
  assert(state[1]!.text === "", "text stays empty");
});

run("completed with only whitespace text is treated as completed_empty", () => {
  let state: Record<number, SuggestionState> = {};
  state = applyResponseSuggestionEvent(state, {
    type: "started", session_id: 1, turn_id: 1, utterance_id: 1, utterance_revision: 1, generation_id: 1,
  });
  state = applyResponseSuggestionEvent(state, {
    type: "completed", session_id: 1, turn_id: 1, utterance_id: 1, utterance_revision: 1, generation_id: 1, text: "   \n",
  });
  assert(state[1]!.status === "completed_empty", "whitespace-only text is completed_empty");
});

run("skipped, cancelled and error need an existing entry for the same generation", () => {
  const empty: Record<number, SuggestionState> = {};
  const afterSkip = applyResponseSuggestionEvent(empty, {
    type: "skipped", session_id: 1, turn_id: 9, utterance_id: 9, utterance_revision: 1, generation_id: 1,
  });
  assert(afterSkip[9] === undefined, "no entry for an unknown utterance/generation");

  let state: Record<number, SuggestionState> = {};
  state = applyResponseSuggestionEvent(state, {
    type: "started", session_id: 1, turn_id: 2, utterance_id: 2, utterance_revision: 1, generation_id: 1,
  });
  state = applyResponseSuggestionEvent(state, {
    type: "error", session_id: 1, turn_id: 2, utterance_id: 2, utterance_revision: 1, generation_id: 1,
    message: "falha de rede",
  });
  assert(state[2]!.status === "error", "status error");
  assert(state[2]!.errorMessage === "falha de rede", "error message captured");
});

run("events for a different utterance do not affect other utterances", () => {
  let state: Record<number, SuggestionState> = {};
  state = applyResponseSuggestionEvent(state, {
    type: "started", session_id: 1, turn_id: 1, utterance_id: 1, utterance_revision: 1, generation_id: 1,
  });
  state = applyResponseSuggestionEvent(state, {
    type: "started", session_id: 1, turn_id: 1, utterance_id: 2, utterance_revision: 1, generation_id: 1,
  });
  state = applyResponseSuggestionEvent(state, {
    type: "cancelled", session_id: 1, turn_id: 1, utterance_id: 1, utterance_revision: 1, generation_id: 1,
  });
  assert(state[1]!.status === "cancelled", "utterance 1 cancelled");
  assert(state[2]!.status === "preparing", "utterance 2 unaffected");
});

run("diagnostics event does not affect visible suggestion state", () => {
  let state: Record<number, SuggestionState> = {};
  state = applyResponseSuggestionEvent(state, {
    type: "started", session_id: 1, turn_id: 1, utterance_id: 1, utterance_revision: 1, generation_id: 1,
  });
  const before = state[1];
  state = applyResponseSuggestionEvent(state, {
    type: "diagnostics", session_id: 1, turn_id: 1, utterance_id: 1, utterance_revision: 1, generation_id: 1,
    event_emitted: "skipped",
  });
  assert(state[1] === before, "diagnostics is a no-op for SuggestionState");
});

run("applyResponseSuggestionDiagnostics records fields keyed by turn_id", () => {
  let diagnostics: Record<number, ResponseSuggestionDiagnostics> = {};
  diagnostics = applyResponseSuggestionDiagnostics(diagnostics, {
    type: "started", session_id: 1,
    turn_id: 1,
    generation_id: 1,
  });
  assert(Object.keys(diagnostics).length === 0, "non-diagnostics events are ignored");

  diagnostics = applyResponseSuggestionDiagnostics(diagnostics, {
    type: "diagnostics", session_id: 1, utterance_id: 1, utterance_revision: 1,
    turn_id: 1,
    generation_id: 1,
    provider: "ollama",
    model: "llama3",
    request_started: 1000,
    http_status: 200,
    first_chunk_received: 1050,
    raw_prefix: "[SKIP]",
    skip_detected: true,
    cancel_reason: null,
    latency_ms: 120,
    final_text_length: 0,
    event_emitted: "skipped",
  });

  const entry = diagnostics[1]!;
  assert(entry.provider === "ollama", "provider recorded");
  assert(entry.http_status === 200, "http_status recorded");
  assert(entry.raw_prefix === "[SKIP]", "raw_prefix recorded");
  assert(entry.skip_detected === true, "skip_detected recorded");
  assert(entry.event_emitted === "skipped", "event_emitted recorded");
});

run("applyResponseSuggestionDiagnostics fills missing optional fields with defaults", () => {
  let diagnostics: Record<number, ResponseSuggestionDiagnostics> = {};
  diagnostics = applyResponseSuggestionDiagnostics(diagnostics, {
    type: "diagnostics", session_id: 1, utterance_id: 5, utterance_revision: 1,
    turn_id: 5,
    generation_id: 1,
  });
  const entry = diagnostics[5]!;
  assert(entry.http_status === null, "missing http_status defaults to null");
  assert(entry.raw_prefix === "", "missing raw_prefix defaults to empty string");
  assert(entry.skip_detected === false, "missing skip_detected defaults to false");
  assert(entry.finalization_reason === "", "missing finalization_reason defaults to empty string");
  assert(entry.gap_ms_used === 0, "missing gap_ms_used defaults to 0");
  assert(entry.silence_detected_ms === null, "missing silence_detected_ms defaults to null");
  assert(
    entry.end_of_speech_to_first_visible_token_ms === null,
    "missing end_of_speech_to_first_visible_token_ms defaults to null",
  );
});

run("applyResponseSuggestionDiagnostics records the new latency and trigger fields", () => {
  let diagnostics: Record<number, ResponseSuggestionDiagnostics> = {};
  diagnostics = applyResponseSuggestionDiagnostics(diagnostics, {
    type: "diagnostics", session_id: 1, utterance_id: 1, utterance_revision: 1,
    turn_id: 1,
    generation_id: 1,
    finalization_reason: "inactivity_timeout",
    gap_ms_used: 1800,
    silence_detected_ms: 1800,
    utterance_finalized_to_request_started_ms: 12,
    request_to_first_http_chunk_ms: 4200,
    request_to_first_visible_token_ms: 4210,
    end_of_speech_to_first_visible_token_ms: 4222,
  });

  const entry = diagnostics[1]!;
  assert(entry.finalization_reason === "inactivity_timeout", "finalization_reason recorded");
  assert(entry.gap_ms_used === 1800, "gap_ms_used recorded");
  assert(entry.silence_detected_ms === 1800, "silence_detected_ms recorded");
  assert(
    entry.utterance_finalized_to_request_started_ms === 12,
    "utterance_finalized_to_request_started_ms recorded",
  );
  assert(
    entry.end_of_speech_to_first_visible_token_ms === 4222,
    "end_of_speech_to_first_visible_token_ms recorded",
  );
});

run("events from another session are ignored by the active-session gate", () => {
  let state: Record<number, SuggestionState> = {};
  state = applyResponseSuggestionEvent(
    state,
    { type: "started", session_id: 1, turn_id: 1, utterance_id: 1, utterance_revision: 1, generation_id: 10 },
    1,
  );
  const after = applyResponseSuggestionEvent(
    state,
    { type: "delta", session_id: 2, turn_id: 1, utterance_id: 1, utterance_revision: 1, generation_id: 10, text: "stale" },
    1,
  );
  assert(after === state, "previous-session event is ignored");
});

run("an old utterance revision is ignored", () => {
  let state: Record<number, SuggestionState> = {};
  state = applyResponseSuggestionEvent(state, {
    type: "started", session_id: 1, turn_id: 1, utterance_id: 1, utterance_revision: 2, generation_id: 11,
  });
  const after = applyResponseSuggestionEvent(state, {
    type: "completed", session_id: 1, turn_id: 1, utterance_id: 1, utterance_revision: 1, generation_id: 11, text: "revision stale",
  });
  assert(after === state, "old revision is ignored");
});

run("a delayed started event cannot replace a newer active generation", () => {
  let state: Record<number, SuggestionState> = {};
  state = applyResponseSuggestionEvent(state, {
    type: "started", session_id: 1, turn_id: 1, utterance_id: 1, utterance_revision: 1, generation_id: 20,
  });
  const after = applyResponseSuggestionEvent(state, {
    type: "started", session_id: 1, turn_id: 1, utterance_id: 1, utterance_revision: 1, generation_id: 19,
  });
  assert(after === state, "older started event is ignored");
});
