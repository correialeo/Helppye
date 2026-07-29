export type QuestionDetectionStatus = "candidate" | "confirmed" | "dismissed";

export interface ConversationTurnRef {
  id: number;
  utterances: number[];
}

export interface QuestionDetectionRef {
  turn_id: number;
  status: QuestionDetectionStatus;
  matched_utterance_ids: number[];
}

export interface QuestionUtteranceRenderState<TTurn extends ConversationTurnRef, TDetection extends QuestionDetectionRef> {
  turn: TTurn | undefined;
  detection: TDetection | undefined;
  usesUtterance: boolean;
  isCandidateQuestion: boolean;
  isConfirmedQuestion: boolean;
}

export function questionRenderStateForUtterance<
  TTurn extends ConversationTurnRef,
  TDetection extends QuestionDetectionRef,
>(
  utteranceId: number,
  turns: TTurn[],
  detectionsByTurnId: Record<number, TDetection>,
): QuestionUtteranceRenderState<TTurn, TDetection> {
  const turn = turns.find((candidate) => candidate.utterances.includes(utteranceId));
  const detection = turn ? detectionsByTurnId[turn.id] : undefined;
  const usesUtterance = detection?.matched_utterance_ids.includes(utteranceId) ?? false;

  return {
    turn,
    detection,
    usesUtterance,
    isCandidateQuestion: usesUtterance && detection?.status === "candidate",
    isConfirmedQuestion: usesUtterance && detection?.status === "confirmed",
  };
}
