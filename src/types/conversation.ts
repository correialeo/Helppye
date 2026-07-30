import type { AudioSourceKind } from "./audio";

export type ConversationSpeaker = "user" | "other_person";

export interface ConversationTurn {
  id: number;
  source: AudioSourceKind;
  speaker: ConversationSpeaker;
  text: string;
  utterances: number[];
  started_at: number;
  ended_at: number;
  finalized_at: number | null;
}

export interface ConversationUtterance {
  id: number;
  source: AudioSourceKind;
  speaker: ConversationSpeaker;
  text: string;
  segments: number[];
  started_at: number;
  ended_at: number;
  finalized_at: number | null;
  revision: number;
}

export type UtteranceFinalizationReason =
  | "inactivity_timeout"
  | "speaker_changed"
  | "source_changed"
  | "capture_stopped"
  | "manual_flush"
  | "session_ended"
  | "maximum_duration";

export interface ConversationTimelineSnapshot {
  turns: ConversationTurn[];
  utterances: ConversationUtterance[];
}

export type ConversationTimelineEvent =
  | {
      type: "turn_started";
      turn_id: number;
      speaker: ConversationSpeaker;
      source: AudioSourceKind;
      started_at: number;
    }
  | { type: "turn_updated"; turn: ConversationTurn }
  | { type: "turn_finalized"; turn: ConversationTurn }
  | {
      type: "utterance_started";
      utterance_id: number;
      turn_id: number;
      speaker: ConversationSpeaker;
      source: AudioSourceKind;
      started_at: number;
    }
  | {
      type: "utterance_updated";
      utterance_id: number;
      turn_id: number;
      speaker: ConversationSpeaker;
      source: AudioSourceKind;
      started_at: number;
      text: string;
      ended_at: number;
      segments: number[];
    }
  | {
      type: "utterance_finalized";
      turn_id: number;
      utterance: ConversationUtterance;
      finalization_reason: UtteranceFinalizationReason;
      gap_ms_used: number;
      silence_detected_ms: number | null;
      session_id: number;
    };
