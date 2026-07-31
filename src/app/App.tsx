import { useEffect } from "react";
import { ErrorBoundary } from "./ErrorBoundary";
import { AppRouter } from "./router";
import { resolveInitialScreen } from "./appFlow";
import { AudioCaptureProvider } from "../hooks/useAudioCapture";
import { useWindowMode } from "../hooks/useWindowMode";
import { useOnboardingStore } from "../stores/useOnboardingStore";
import { SessionScreen } from "../features/session/SessionScreen";
import { SettingsScreen } from "../features/settings/SettingsScreen";
import { getSessionWindowRole, getWindowStartedAt, openSettingsWindow } from "../services/sessionWindowService";

export default function App() {
  const screen = useOnboardingStore((s) => s.screen);
  const windowRole = getSessionWindowRole();

  useEffect(() => {
    if (windowRole !== "main") return;
    const state = useOnboardingStore.getState();
    const resolved = resolveInitialScreen({
      onboardingComplete: state.onboardingComplete,
      screen: state.screen,
    });
    if (resolved !== state.screen) state.setScreen(resolved);
  }, [windowRole]);

  useWindowMode(screen, windowRole === "main");

  if (windowRole !== "main") {
    if (windowRole === "settings") {
      return (
        <ErrorBoundary>
          <AudioCaptureProvider />
          <SettingsScreen onBack={() => window.close()} onOpenDeveloperTools={() => {}} />
        </ErrorBoundary>
      );
    }

    return (
      <ErrorBoundary>
        <AudioCaptureProvider />
        <SessionScreen
          mode={windowRole}
          startedAt={getWindowStartedAt()}
          onOpenSettings={() => void openSettingsWindow()}
          onOpenDeveloperTools={() => {}}
          onEndSession={() => window.close()}
        />
      </ErrorBoundary>
    );
  }

  return (
    <ErrorBoundary>
      <AudioCaptureProvider />
      <AppRouter />
    </ErrorBoundary>
  );
}
