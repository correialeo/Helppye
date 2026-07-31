import { useState } from "react";
import { nextOnboardingScreen, type AppScreen } from "./appFlow";
import { useOnboardingStore } from "../stores/useOnboardingStore";
import { startCapture, stopCapture } from "../services/audioService";
import { endConversationSession, startConversationSession } from "../services/conversationService";
import { closeSessionWindows, openSessionWindows } from "../services/sessionWindowService";
import { WelcomeScreen } from "../features/welcome/WelcomeScreen";
import { CloudLoginScreen } from "../features/welcome/CloudLoginScreen";
import { ReadyScreen } from "../features/ready/ReadyScreen";
import { SessionScreen } from "../features/session/SessionScreen";
import { SettingsScreen } from "../features/settings/SettingsScreen";
import { DeveloperToolsScreen } from "../features/developer-tools/DeveloperToolsScreen";
import { SetupScreen } from "../features/setup/SetupScreen";

async function stopAllCapture() {
  await Promise.allSettled([stopCapture("microphone"), stopCapture("system_output")]);
}

export function AppRouter() {
  const screen = useOnboardingStore((s) => s.screen);
  const setScreen = useOnboardingStore((s) => s.setScreen);
  const completeOnboarding = useOnboardingStore((s) => s.completeOnboarding);
  const [sessionStartedAt, setSessionStartedAt] = useState(0);
  const [sessionDetached, setSessionDetached] = useState(false);
  const [settingsReturnTo, setSettingsReturnTo] = useState<AppScreen>("ready");
  const [developerToolsOpen, setDeveloperToolsOpen] = useState(false);

  const goNext = () => setScreen(nextOnboardingScreen(screen));

  const openSettings = () => {
    setSettingsReturnTo(screen === "session" ? "session" : "ready");
    setScreen("settings");
  };

  const openDeveloperTools = () => setDeveloperToolsOpen(true);

  const startSession = async () => {
    await startConversationSession().catch((e) => {
      console.error("falha ao abrir fronteira de sessao", e);
    });
    await Promise.allSettled([startCapture("microphone"), startCapture("system_output")]);

    const startedAt = Date.now();
    setSessionStartedAt(startedAt);
    setSessionDetached(await openSessionWindows(startedAt));
    setScreen("session");
  };

  const endSession = async () => {
    await closeSessionWindows();
    setSessionDetached(false);
    await stopAllCapture();
    await endConversationSession().catch(() => {});
    setScreen("ready");
  };

  const finishOnboarding = async () => {
    await stopAllCapture();
    completeOnboarding();
  };

  if (developerToolsOpen) {
    return <DeveloperToolsScreen onBack={() => setDeveloperToolsOpen(false)} />;
  }

  switch (screen) {
    case "welcome":
      return <WelcomeScreen onContinueWithoutLogin={goNext} onLogin={() => setScreen("cloud-login")} />;
    case "cloud-login":
      return (
        <CloudLoginScreen
          onContinueWithoutLogin={() => setScreen(nextOnboardingScreen("welcome"))}
          onBack={() => setScreen("welcome")}
        />
      );
    case "profile":
      return <SetupScreen onBack={() => setScreen("welcome")} onComplete={finishOnboarding} />;
    case "language":
      return <SetupScreen onBack={() => setScreen("welcome")} onComplete={finishOnboarding} />;
    case "permissions":
      return <SetupScreen onBack={() => setScreen("welcome")} onComplete={finishOnboarding} />;
    case "audio-setup":
      return <SetupScreen onBack={() => setScreen("welcome")} onComplete={finishOnboarding} />;
    case "ai-provider":
      return <SetupScreen onBack={() => setScreen("welcome")} onComplete={finishOnboarding} />;
    case "onboarding-review":
      return <SetupScreen onBack={() => setScreen("welcome")} onComplete={finishOnboarding} />;
    case "ready":
      return <ReadyScreen onStartSession={startSession} onOpenSettings={openSettings} />;
    case "session":
      return (
        <SessionScreen
          mode={sessionDetached ? "coordinator" : "combined"}
          startedAt={sessionStartedAt}
          onOpenSettings={openSettings}
          onOpenDeveloperTools={openDeveloperTools}
          onEndSession={endSession}
        />
      );
    case "settings":
      return <SettingsScreen onBack={() => setScreen(settingsReturnTo)} onOpenDeveloperTools={openDeveloperTools} />;
  }
}
