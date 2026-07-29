import { applyResponseSuggestionEvent, SuggestionState } from "./responseSuggestionViewModel";

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
    { 1: { generationId: 3, status: "completed", text: "old" } },
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

run("completed replaces accumulated text with the final text", () => {
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
  assert(state[1]!.status === "completed", "status completed");
  assert(state[1]!.text === "texto final completo", "text is the final text");
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
