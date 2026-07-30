import { create } from "zustand";
import { createJSONStorage, persist } from "zustand/middleware";
import type { AppScreen } from "../app/appFlow";

export type LanguageCode = "pt-BR";

interface OnboardingState {
  onboardingComplete: boolean;
  screen: AppScreen;
  userName: string;
  language: LanguageCode;
  devMode: boolean;
  setScreen: (screen: AppScreen) => void;
  setUserName: (name: string) => void;
  setLanguage: (language: LanguageCode) => void;
  setDevMode: (enabled: boolean) => void;
  completeOnboarding: () => void;
}

/**
 * Everything here is genuinely UI-only state with no backend equivalent — device
 * selection and provider config are already persisted server-side (`device_selection.json`,
 * `response_provider.json` via their respective Rust commands) and are read from there,
 * not duplicated here, to avoid two sources of truth drifting apart. See
 * docs/frontend-architecture.md §Persistência.
 *
 * API keys are never part of this store, never touch localStorage, and never will —
 * they only ever go to the OS keychain via `services/responseProviderService.ts`.
 */
export const useOnboardingStore = create<OnboardingState>()(
  persist(
    (set) => ({
      onboardingComplete: false,
      screen: "welcome",
      userName: "",
      language: "pt-BR",
      devMode: false,
      setScreen: (screen) => set({ screen }),
      setUserName: (userName) => set({ userName: userName.trim() }),
      setLanguage: (language) => set({ language }),
      setDevMode: (devMode) => set({ devMode }),
      completeOnboarding: () => set({ onboardingComplete: true, screen: "ready" }),
    }),
    {
      name: "helppye.onboarding",
      storage: createJSONStorage(() => localStorage),
      // Explicit allowlist, not "everything in this store" — the next field added here
      // has to be deliberately opted into persistence, not persisted by default.
      partialize: (state) => ({
        onboardingComplete: state.onboardingComplete,
        screen: state.screen,
        userName: state.userName,
        language: state.language,
        devMode: state.devMode,
      }),
    },
  ),
);
