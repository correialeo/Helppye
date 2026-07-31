import { createSessionToggleController } from "./sessionToggleController";
import type { AppScreen } from "./appFlow";

function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(`assertion failed: ${message}`);
}

async function run(name: string, fn: () => Promise<void>): Promise<void> {
  await fn();
  console.log(`ok: ${name}`);
}

await run("global Ctrl/Cmd+D starts when no session is active and ends when session is active", async () => {
  let screen: AppScreen = "ready";
  const calls: string[] = [];
  const toggle = createSessionToggleController({
    getScreen: () => screen,
    startSession: async () => {
      calls.push("start");
      screen = "session";
    },
    endSession: async () => {
      calls.push("end");
      screen = "ready";
    },
  });

  await toggle();
  await toggle();

  assert(calls.join(",") === "start,end", "toggle alternates start/end");
});

await run("two rapid global Ctrl/Cmd+D presses never open multiple sessions", async () => {
  let screen: AppScreen = "ready";
  let releaseStart!: () => void;
  const firstStart = new Promise<void>((resolve) => {
    releaseStart = resolve;
  });
  let starts = 0;
  const toggle = createSessionToggleController({
    getScreen: () => screen,
    startSession: async () => {
      starts += 1;
      await firstStart;
      screen = "session";
    },
    endSession: async () => {
      screen = "ready";
    },
  });

  const first = toggle();
  const second = toggle();
  releaseStart();
  await Promise.all([first, second]);

  assert(starts === 1, "second press is ignored while start is in flight");
});
