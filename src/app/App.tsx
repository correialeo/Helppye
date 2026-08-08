import { useEffect, useState } from "react";
import { ErrorBoundary } from "./ErrorBoundary";
import { AppRouter } from "./router";
import { resolveInitialScreen } from "./appFlow";
import { AudioCaptureProvider } from "../hooks/useAudioCapture";
import { useWindowMode } from "../hooks/useWindowMode";
import { useOnboardingStore } from "../stores/useOnboardingStore";
import { SessionScreen } from "../features/session/SessionScreen";
import { SettingsScreen } from "../features/settings/SettingsScreen";
import { DeveloperToolsScreen } from "../features/developer-tools/DeveloperToolsScreen";
import { getSessionWindowRole, getWindowStartedAt, openSettingsWindow, requestSessionEnd } from "../services/sessionWindowService";

export default function App() {
  const screen = useOnboardingStore((s) => s.screen);
  const windowRole = getSessionWindowRole();
  const [settingsDiagnosticsOpen, setSettingsDiagnosticsOpen] = useState(false);
  const [sessionDiagnosticsOpen, setSessionDiagnosticsOpen] = useState(false);

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
      if (settingsDiagnosticsOpen) {
        return (
          <ErrorBoundary>
            <AudioCaptureProvider />
            <DeveloperToolsScreen onBack={() => setSettingsDiagnosticsOpen(false)} />
          </ErrorBoundary>
        );
      }

      return (
        <ErrorBoundary>
          <AudioCaptureProvider />
          <SettingsScreen onBack={() => window.close()} onOpenDeveloperTools={() => setSettingsDiagnosticsOpen(true)} />
        </ErrorBoundary>
      );
    }

    if (sessionDiagnosticsOpen) {
      return (
        <ErrorBoundary>
          <AudioCaptureProvider />
          <DeveloperToolsScreen onBack={() => setSessionDiagnosticsOpen(false)} />
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
          onOpenDeveloperTools={() => setSessionDiagnosticsOpen(true)}
          onEndSession={() => void requestSessionEnd()}
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
