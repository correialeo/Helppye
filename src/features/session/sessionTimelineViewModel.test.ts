import { buildSessionExchanges } from "./sessionTimelineViewModel";
import type { ConversationTurn, ConversationUtterance } from "../../types/conversation";

function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(`assertion failed: ${message}`);
}

function run(name: string, fn: () => void): void {
  fn();
  console.log(`ok: ${name}`);
}

function utterance(
  id: number,
  speaker: "user" | "other_person",
  startedAt: number,
  text: string,
  receivedSequence = id,
): ConversationUtterance {
  return {
    id,
    speaker,
    source: speaker === "user" ? "microphone" : "system_output",
    text,
    segments: [id],
    received_sequence: receivedSequence,
    started_at: startedAt,
    ended_at: startedAt + 100,
    finalized_at: startedAt + 100,
    revision: 1,
  };
}

function turn(id: number, speaker: "user" | "other_person", utterances: number[]): ConversationTurn {
  return {
    id,
    speaker,
    source: speaker === "user" ? "microphone" : "system_output",
    text: String(id),
    utterances,
    started_at: id * 1000,
    ended_at: id * 1000 + 100,
    finalized_at: null,
  };
}

run("session exchanges preserve chronological utterance order instead of grouping by turn", () => {
  const turns = [turn(1, "other_person", [1, 3, 4]), turn(2, "user", [2]), turn(3, "other_person", [5])];
  const utterances = [
    utterance(1, "other_person", 100, "OTHER 1"),
    utterance(2, "user", 200, "YOU 1"),
    utterance(3, "other_person", 300, "OTHER 2"),
    utterance(4, "other_person", 400, "OTHER 3"),
    utterance(5, "other_person", 500, "OTHER 4"),
  ];

  const exchanges = buildSessionExchanges(turns, utterances, {});

  assert(
    exchanges.map((exchange) => exchange.utteranceId).join(",") === "1,3,4,5",
    "remote exchanges follow utterance chronology",
  );
  assert(exchanges[0]!.turnId === 1 && exchanges[3]!.turnId === 3, "turn lookup remains intact");
});

run("session exchanges follow received sequence when source timestamps are biased", () => {
  const turns = [turn(1, "other_person", [1, 3]), turn(2, "user", [2])];
  const utterances = [
    utterance(1, "other_person", 100, "OTHER 1", 1),
    utterance(3, "other_person", 110, "OTHER 2", 3),
    utterance(2, "user", 10_000, "YOU 1", 2),
  ].sort((a, b) => a.received_sequence - b.received_sequence);

  const exchanges = buildSessionExchanges(turns, utterances, {});

  assert(exchanges.map((exchange) => exchange.utteranceId).join(",") === "1,3", "remote exchanges keep feed order");
});
