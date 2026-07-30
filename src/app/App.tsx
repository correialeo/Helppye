import { useEffect } from "react";
import { ErrorBoundary } from "./ErrorBoundary";
import { AppRouter } from "./router";
import { resolveInitialScreen } from "./appFlow";
import { AudioCaptureProvider } from "../hooks/useAudioCapture";
import { useWindowMode } from "../hooks/useWindowMode";
import { useOnboardingStore } from "../stores/useOnboardingStore";

/**
 * Owns only: startup normalization, global providers, window sizing, and the error
 * boundary. Everything about *what screen renders* lives in `app/router.tsx`; everything
 * about *what a screen looks like* lives in `features/`. See docs/frontend-architecture.md.
 */
export default function App() {
  const screen = useOnboardingStore((s) => s.screen);

  useEffect(() => {
    const state = useOnboardingStore.getState();
    const resolved = resolveInitialScreen({ onboardingComplete: state.onboardingComplete, screen: state.screen });
    if (resolved !== state.screen) state.setScreen(resolved);
    // Runs once, right after the persisted store rehydrates — not on every screen change.
  }, []);

  useWindowMode(screen);

  return (
    <ErrorBoundary>
      <AudioCaptureProvider />
      <AppRouter />
    </ErrorBoundary>
  );
}
