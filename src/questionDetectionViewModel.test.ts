import { questionRenderStateForUtterance } from "./questionDetectionViewModel";

function assert(condition: boolean, message: string): void {
  if (!condition) {
    throw new Error(message);
  }
}

const turns = [
  { id: 7, utterances: [101, 102] },
  { id: 8, utterances: [201] },
];

const detections = {
  7: {
    turn_id: 7,
    status: "confirmed" as const,
    matched_utterance_ids: [102],
  },
};

const matched = questionRenderStateForUtterance(102, turns, detections);
assert(matched.turn?.id === 7, "locates turn by utterance membership");
assert(matched.detection?.turn_id === 7, "locates detection by turn_id");
assert(matched.usesUtterance, "matches related utterance id");
assert(matched.isConfirmedQuestion, "highlights confirmed related utterance");

const unrelated = questionRenderStateForUtterance(101, turns, detections);
assert(unrelated.turn?.id === 7, "keeps turn association for unrelated utterance");
assert(!unrelated.usesUtterance, "does not compare turn_id directly with utterance.id");
assert(!unrelated.isConfirmedQuestion, "does not highlight unrelated utterance in same turn");
