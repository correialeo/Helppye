import { create } from "zustand";
import type { AudioDevice, AudioSourceKind, ResolutionSource, ResolvedDevice } from "../types/audio";

export type CaptureStatus =
  | { kind: "idle" }
  | { kind: "capturing" }
  | { kind: "switching" }
  | { kind: "disconnected" }
  | { kind: "error"; message: string };

export interface SourceCaptureState {
  status: CaptureStatus;
  levelDb: number;
  devices: AudioDevice[];
  selectedId: string | null;
  selectionSource: ResolutionSource | null;
  suggestedDevice: ResolvedDevice | null;
}

interface AudioCaptureStore {
  microphone: SourceCaptureState;
  system_output: SourceCaptureState;
  patch: (source: AudioSourceKind, patch: Partial<SourceCaptureState>) => void;
}

const initialSourceState: SourceCaptureState = {
  status: { kind: "idle" },
  levelDb: -Infinity,
  devices: [],
  selectedId: null,
  selectionSource: null,
  suggestedDevice: null,
};

/**
 * In-memory only (no `persist`) and deliberately global rather than local component
 * state: permissions, audio-setup, session, and settings all show/control the same two
 * capture sources, and re-subscribing a fresh `audio://capture-event` listener on every
 * screen mount would miss the `started` event fired by whichever earlier screen actually
 * started capture. A single app-level subscription (see hooks/useAudioCapture.ts
 * `AudioCaptureProvider`) keeps this store current; every screen just reads it.
 */
export const useAudioCaptureStore = create<AudioCaptureStore>((set) => ({
  microphone: { ...initialSourceState },
  system_output: { ...initialSourceState },
  patch: (source, patch) =>
    set(
      (state) =>
        ({
          [source]: { ...state[source], ...patch },
        }) as Partial<AudioCaptureStore>,
    ),
}));
