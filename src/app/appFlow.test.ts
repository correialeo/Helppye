import {
  nextOnboardingScreen,
  ONBOARDING_STEPS,
  onboardingStepIndex,
  previousOnboardingScreen,
  resolveInitialScreen,
} from "./appFlow";

function assert(condition: boolean, message: string): void {
  if (!condition) {
    throw new Error(`assertion failed: ${message}`);
  }
}

function run(name: string, fn: () => void): void {
  fn();
  console.log(`ok: ${name}`);
}

run("onboarding steps start at welcome and end at onboarding-review", () => {
  assert(ONBOARDING_STEPS[0] === "welcome", "first step is welcome");
  assert(ONBOARDING_STEPS[ONBOARDING_STEPS.length - 1] === "onboarding-review", "last step is onboarding-review");
});

run("nextOnboardingScreen walks the whole sequence in order", () => {
  let screen = ONBOARDING_STEPS[0]!;
  for (let i = 1; i < ONBOARDING_STEPS.length; i++) {
    screen = nextOnboardingScreen(screen);
    assert(screen === ONBOARDING_STEPS[i], `step ${i} is ${ONBOARDING_STEPS[i]}, got ${screen}`);
  }
});

run("nextOnboardingScreen does not advance past the last step", () => {
  assert(nextOnboardingScreen("onboarding-review") === "onboarding-review", "clamped at the last step");
});

run("previousOnboardingScreen does not regress before the first step", () => {
  assert(previousOnboardingScreen("welcome") === "welcome", "clamped at the first step");
});

run("previousOnboardingScreen undoes nextOnboardingScreen", () => {
  assert(previousOnboardingScreen(nextOnboardingScreen("profile")) === "profile", "round trip returns to profile");
});

run("onboardingStepIndex matches the step's position", () => {
  assert(onboardingStepIndex("welcome") === 0, "welcome is step 0");
  assert(onboardingStepIndex("ai-provider") === 5, "ai-provider is step 5");
});

run("onboardingStepIndex is 0 for screens outside the onboarding sequence", () => {
  assert(onboardingStepIndex("session") === 0, "session isn't part of the dotted sequence");
  assert(onboardingStepIndex("settings") === 0, "settings isn't part of the dotted sequence");
});

run("resolveInitialScreen sends completed onboarding straight to ready", () => {
  const resolved = resolveInitialScreen({ onboardingComplete: true, screen: "profile" });
  assert(resolved === "ready", "a stale mid-onboarding screen is never resumed once onboarding is done");
});

run("resolveInitialScreen never cold-opens directly into session", () => {
  const resolved = resolveInitialScreen({ onboardingComplete: false, screen: "session" });
  assert(resolved === "welcome", "a session screen persisted from a previous run must not auto-resume capture");
});

run("resolveInitialScreen never cold-opens directly into ready from an incomplete run", () => {
  const resolved = resolveInitialScreen({ onboardingComplete: false, screen: "ready" });
  assert(resolved === "welcome", "onboardingComplete=false always wins over a persisted ready/session screen");
});

run("resolveInitialScreen resumes an unfinished onboarding at its last screen", () => {
  const resolved = resolveInitialScreen({ onboardingComplete: false, screen: "ai-provider" });
  assert(resolved === "ai-provider", "an in-progress onboarding picks up where it left off");
});
