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

run("started resets state for the turn", () => {
  const state = applyResponseSuggestionEvent(
    { 1: { generationId: 3, status: "completed_with_text", text: "old" } },
    { type: "started", turn_id: 1, generation_id: 4 },
  );
  assert(state[1]!.generationId === 4, "generation id updated");
  assert(state[1]!.status === "streaming", "status is streaming");
  assert(state[1]!.text === "", "text reset");
});

run("delta appends text for the current generation", () => {
  let state: Record<number, SuggestionState> = {};
  state = applyResponseSuggestionEvent(state, { type: "started", turn_id: 1, generation_id: 1 });
  state = applyResponseSuggestionEvent(state, {
    type: "delta",
    turn_id: 1,
    generation_id: 1,
    text: "Olá",
  });
  state = applyResponseSuggestionEvent(state, {
    type: "delta",
    turn_id: 1,
    generation_id: 1,
    text: ", tudo bem?",
  });
  assert(state[1]!.text === "Olá, tudo bem?", "text accumulated across deltas");
});

run("stale delta from a superseded generation is ignored", () => {
  let state: Record<number, SuggestionState> = {};
  state = applyResponseSuggestionEvent(state, { type: "started", turn_id: 1, generation_id: 1 });
  state = applyResponseSuggestionEvent(state, { type: "started", turn_id: 1, generation_id: 2 });
  state = applyResponseSuggestionEvent(state, {
    type: "delta",
    turn_id: 1,
    generation_id: 1,
    text: "texto antigo",
  });
  assert(state[1]!.generationId === 2, "still on the newer generation");
  assert(state[1]!.text === "", "stale delta had no effect");
});

run("completed with text replaces accumulated text and is completed_with_text", () => {
  let state: Record<number, SuggestionState> = {};
  state = applyResponseSuggestionEvent(state, { type: "started", turn_id: 1, generation_id: 1 });
  state = applyResponseSuggestionEvent(state, {
    type: "delta",
    turn_id: 1,
    generation_id: 1,
    text: "parcial",
  });
  state = applyResponseSuggestionEvent(state, {
    type: "completed",
    turn_id: 1,
    generation_id: 1,
    text: "texto final completo",
  });
  assert(state[1]!.status === "completed_with_text", "status completed_with_text");
  assert(state[1]!.text === "texto final completo", "text is the final text");
});

run("completed with empty text is a distinct completed_empty status, not skipped", () => {
  let state: Record<number, SuggestionState> = {};
  state = applyResponseSuggestionEvent(state, { type: "started", turn_id: 1, generation_id: 1 });
  state = applyResponseSuggestionEvent(state, {
    type: "completed",
    turn_id: 1,
    generation_id: 1,
    text: "",
  });
  assert(state[1]!.status === "completed_empty", "status completed_empty");
  assert(state[1]!.text === "", "text stays empty");
});

run("completed with only whitespace text is treated as completed_empty", () => {
  let state: Record<number, SuggestionState> = {};
  state = applyResponseSuggestionEvent(state, { type: "started", turn_id: 1, generation_id: 1 });
  state = applyResponseSuggestionEvent(state, {
    type: "completed",
    turn_id: 1,
    generation_id: 1,
    text: "   \n",
  });
  assert(state[1]!.status === "completed_empty", "whitespace-only text is completed_empty");
});

run("skipped, cancelled and error update status without an existing entry being a no-op", () => {
  const empty: Record<number, SuggestionState> = {};
  const afterSkip = applyResponseSuggestionEvent(empty, {
    type: "skipped",
    turn_id: 9,
    generation_id: 1,
  });
  assert(afterSkip[9] === undefined, "no entry for an unknown turn/generation");

  let state: Record<number, SuggestionState> = {};
  state = applyResponseSuggestionEvent(state, { type: "started", turn_id: 2, generation_id: 1 });
  state = applyResponseSuggestionEvent(state, {
    type: "error",
    turn_id: 2,
    generation_id: 1,
    message: "falha de rede",
  });
  assert(state[2]!.status === "error", "status error");
  assert(state[2]!.errorMessage === "falha de rede", "error message captured");
});

run("events for a different turn do not affect other turns", () => {
  let state: Record<number, SuggestionState> = {};
  state = applyResponseSuggestionEvent(state, { type: "started", turn_id: 1, generation_id: 1 });
  state = applyResponseSuggestionEvent(state, { type: "started", turn_id: 2, generation_id: 1 });
  state = applyResponseSuggestionEvent(state, {
    type: "cancelled",
    turn_id: 1,
    generation_id: 1,
  });
  assert(state[1]!.status === "cancelled", "turn 1 cancelled");
  assert(state[2]!.status === "streaming", "turn 2 unaffected");
});

run("diagnostics event does not affect visible suggestion state", () => {
  let state: Record<number, SuggestionState> = {};
  state = applyResponseSuggestionEvent(state, { type: "started", turn_id: 1, generation_id: 1 });
  const before = state[1];
  state = applyResponseSuggestionEvent(state, {
    type: "diagnostics",
    turn_id: 1,
    generation_id: 1,
    event_emitted: "skipped",
  });
  assert(state[1] === before, "diagnostics is a no-op for SuggestionState");
});

run("applyResponseSuggestionDiagnostics records fields keyed by turn_id", () => {
  let diagnostics: Record<number, ResponseSuggestionDiagnostics> = {};
  diagnostics = applyResponseSuggestionDiagnostics(diagnostics, {
    type: "started",
    turn_id: 1,
    generation_id: 1,
  });
  assert(Object.keys(diagnostics).length === 0, "non-diagnostics events are ignored");

  diagnostics = applyResponseSuggestionDiagnostics(diagnostics, {
    type: "diagnostics",
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
    type: "diagnostics",
    turn_id: 5,
    generation_id: 1,
  });
  const entry = diagnostics[5]!;
  assert(entry.http_status === null, "missing http_status defaults to null");
  assert(entry.raw_prefix === "", "missing raw_prefix defaults to empty string");
  assert(entry.skip_detected === false, "missing skip_detected defaults to false");
});
