import type { AppScreen } from "./appFlow";

interface SessionToggleControllerOptions {
  getScreen: () => AppScreen;
  startSession: () => Promise<void>;
  endSession: () => Promise<void>;
}

export function createSessionToggleController({
  getScreen,
  startSession,
  endSession,
}: SessionToggleControllerOptions): () => Promise<void> {
  let inFlight = false;

  return async () => {
    if (inFlight) return;
    inFlight = true;
    try {
      if (getScreen() === "session") {
        await endSession();
      } else {
        await startSession();
      }
    } finally {
      inFlight = false;
    }
  };
}
