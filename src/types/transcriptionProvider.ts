export type TranscriptionProviderId =
  | "whisper_local"
  | "openai_realtime"
  | "google_gemini"
  | "openai_compatible"
  | "fake";

export interface TranscriptionCapabilities {
  local: boolean;
  streaming: boolean;
  partial_results: boolean;
  speaker_source_preserved: boolean;
  language_selection: boolean;
  automatic_language_detection: boolean;
  requires_credentials: boolean;
}

export interface TranscriptionProviderDescriptor {
  id: TranscriptionProviderId;
  display_name: string;
  capabilities: TranscriptionCapabilities;
  available: boolean;
  unavailable_reason: string | null;
}

export type TranscriptionLanguage =
  | { mode: "automatic" }
  | { mode: "fixed"; tag: string };

export interface TranscriptionSettings {
  provider: TranscriptionProviderId;
  language: TranscriptionLanguage;
  model?: string | null;
  providers: {
    whisper_local: { model: string | null };
    google_gemini: {
      model: string;
      endpoint: string;
      audio_chunk_ms: 20 | 40;
      manual_activity_end_silence_ms: 500 | 600 | 700 | 800;
      transcript_drain_ms: number;
      finalization_timeout_ms: number;
    };
    openai_realtime: { model: string | null };
    openai_compatible: { model: string | null; endpoint: string | null };
  };
}

export type TranscriptionConnectionState =
  | "not_configured"
  | "connecting"
  | "connected"
  | "error";
