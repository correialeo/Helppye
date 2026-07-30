import { useState } from "react";
import { nextOnboardingScreen, previousOnboardingScreen, type AppScreen } from "./appFlow";
import { useOnboardingStore } from "../stores/useOnboardingStore";
import { startCapture, stopCapture } from "../services/audioService";
import { endConversationSession, startConversationSession } from "../services/conversationService";

import { WelcomeScreen } from "../features/welcome/WelcomeScreen";
import { CloudLoginScreen } from "../features/welcome/CloudLoginScreen";
import { ProfileScreen } from "../features/profile/ProfileScreen";
import { LanguageScreen } from "../features/language/LanguageScreen";
import { PermissionsScreen } from "../features/permissions/PermissionsScreen";
import { AudioSetupScreen } from "../features/audio-setup/AudioSetupScreen";
import { AiProviderScreen } from "../features/ai-provider/AiProviderScreen";
import { OnboardingReviewScreen } from "../features/onboarding-review/OnboardingReviewScreen";
import { ReadyScreen } from "../features/ready/ReadyScreen";
import { SessionScreen } from "../features/session/SessionScreen";
import { SettingsScreen } from "../features/settings/SettingsScreen";
import { DeveloperToolsScreen } from "../features/developer-tools/DeveloperToolsScreen";

/** Best-effort: capture may already be stopped, or never started — errors here are not
 * worth surfacing to the user, the outcome (idle, not capturing) is the same either way. */
async function stopAllCapture() {
  await Promise.allSettled([stopCapture("microphone"), stopCapture("system_output")]);
}

/**
 * The whole app's navigation graph in one place — every screen receives plain callback
 * props, never reaches into routing itself. See docs/frontend-architecture.md.
 *
 * Two pieces of state live here instead of `useOnboardingStore` because they're
 * transient, not resumable across a relaunch: `sessionStartedAt` (the session timer
 * anchor) and `developerToolsOpen` (an overlay, not a distinct `AppScreen` — see
 * docs/frontend-architecture.md §Ferramentas de desenvolvedor for why it isn't one of
 * the eleven values `AppScreen` is deliberately kept to).
 */
export function AppRouter() {
  const screen = useOnboardingStore((s) => s.screen);
  const setScreen = useOnboardingStore((s) => s.setScreen);
  const completeOnboarding = useOnboardingStore((s) => s.completeOnboarding);

  const [sessionStartedAt, setSessionStartedAt] = useState(0);
  const [settingsReturnTo, setSettingsReturnTo] = useState<AppScreen>("ready");
  const [developerToolsOpen, setDeveloperToolsOpen] = useState(false);

  const goToStep = (target: AppScreen) => setScreen(target);
  const goNext = () => setScreen(nextOnboardingScreen(screen));
  const goBack = () => setScreen(previousOnboardingScreen(screen));

  const openSettings = () => {
    setSettingsReturnTo(screen === "session" ? "session" : "ready");
    setScreen("settings");
  };

  const openDeveloperTools = () => setDeveloperToolsOpen(true);

  const startSession = async () => {
    await stopAllCapture();
    // A fronteira é aberta antes da captura: nenhum áudio da nova sessão pode chegar à
    // timeline enquanto o backend ainda estiver com o estado da sessão anterior.
    // A falha não impede a sessão de começar, mas não pode ser engolida em silêncio: sem
    // a fronteira o backend segue na sessão anterior, e isso precisa aparecer em algum
    // lugar em vez de virar um sintoma inexplicável na tela.
    await startConversationSession().catch((e) => {
      console.error("falha ao abrir a fronteira de sessão", e);
    });
    await Promise.allSettled([startCapture("microphone"), startCapture("system_output")]);
    setSessionStartedAt(Date.now());
    setScreen("session");
  };

  const endSession = async () => {
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
      return <ProfileScreen onBack={goBack} onContinue={goNext} />;
    case "language":
      return <LanguageScreen onBack={goBack} onContinue={goNext} />;
    case "permissions":
      return <PermissionsScreen onBack={goBack} onContinue={goNext} />;
    case "audio-setup":
      return <AudioSetupScreen onBack={goBack} onContinue={goNext} />;
    case "ai-provider":
      return <AiProviderScreen onBack={goBack} onContinue={goNext} />;
    case "onboarding-review":
      return <OnboardingReviewScreen onBack={goBack} onContinue={finishOnboarding} onEdit={goToStep} />;
    case "ready":
      return <ReadyScreen onStartSession={startSession} onOpenSettings={openSettings} />;
    case "session":
      return (
        <SessionScreen
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
