/**
 * The whole app is modeled as a single explicit screen, never scattered booleans (see
 * docs/frontend-architecture.md). `AppScreen` is the entire set of places the app can be.
 */
export type AppScreen =
  | "welcome"
  | "cloud-login"
  | "profile"
  | "language"
  | "permissions"
  | "audio-setup"
  | "ai-provider"
  | "onboarding-review"
  | "ready"
  | "session"
  | "settings";

/** The onboarding sequence, in order — everything between "welcome" and "ready".
 * "cloud-login" is intentionally excluded: it's a detour from "welcome", not a step you
 * progress through, so it doesn't get a dot in ProgressDots. */
export const ONBOARDING_STEPS: readonly AppScreen[] = [
  "welcome",
  "profile",
  "language",
  "permissions",
  "audio-setup",
  "ai-provider",
  "onboarding-review",
];

export function onboardingStepIndex(screen: AppScreen): number {
  const index = ONBOARDING_STEPS.indexOf(screen);
  return index === -1 ? 0 : index;
}

export function nextOnboardingScreen(screen: AppScreen): AppScreen {
  const index = onboardingStepIndex(screen);
  return ONBOARDING_STEPS[Math.min(index + 1, ONBOARDING_STEPS.length - 1)] ?? screen;
}

export function previousOnboardingScreen(screen: AppScreen): AppScreen {
  const index = onboardingStepIndex(screen);
  return ONBOARDING_STEPS[Math.max(index - 1, 0)] ?? screen;
}

/**
 * What screen the app should actually open on, given what was persisted from the last
 * run. Pure on purpose (see hooks/useOnboardingStore.ts) so it's covered by
 * responseSuggestionViewModel-style logic tests without touching Zustand or Tauri.
 *
 * Two deliberate safety rules, not just "resume where you left off":
 * - Onboarding already complete → always land on "ready", never resume a stale
 *   mid-onboarding screen from a previous, unfinished run.
 * - Never cold-open directly into "session" — even if the app closed mid-session, a
 *   fresh launch must not silently start capturing audio again; the user asks for that
 *   explicitly from "ready" (click or Ctrl/Cmd+D).
 */
export function resolveInitialScreen(persisted: { onboardingComplete: boolean; screen: AppScreen }): AppScreen {
  if (persisted.onboardingComplete) return "ready";
  if (persisted.screen === "ready" || persisted.screen === "session") return "welcome";
  return persisted.screen;
}
