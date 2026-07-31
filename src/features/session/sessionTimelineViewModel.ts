import type { ConversationTurn, ConversationUtterance } from "../../types/conversation";
import type { SuggestionState } from "./responseSuggestionViewModel";

export interface SessionExchange {
  utteranceId: number;
  turnId: number;
  question: string;
  suggestion: SuggestionState | undefined;
}

function isEligible(item: { speaker: string; source: string }): boolean {
  return item.speaker === "other_person" && item.source === "system_output";
}

export function buildSessionExchanges(
  turns: ConversationTurn[],
  utterances: ConversationUtterance[],
  suggestions: Record<number, SuggestionState>,
): SessionExchange[] {
  const turnOf = new Map<number, number>();
  for (const turn of turns) {
    for (const utteranceId of turn.utterances) turnOf.set(utteranceId, turn.id);
  }

  return utterances
    .filter((utterance) => isEligible(utterance) && utterance.text.trim().length > 0)
    .map((utterance) => ({
      utteranceId: utterance.id,
      turnId: turnOf.get(utterance.id) ?? suggestions[utterance.id]?.turnId ?? -1,
      question: utterance.text,
      suggestion: suggestions[utterance.id],
    }));
}
