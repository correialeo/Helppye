import { WebviewWindow } from "@tauri-apps/api/webviewWindow";

export type SessionWindowRole = "main" | "ai" | "chat";

const AI_WINDOW = "helppye-ai-response";
const CHAT_WINDOW = "helppye-session-chat";

function isTauriRuntime(): boolean {
  return "__TAURI_INTERNALS__" in window;
}

export function getSessionWindowRole(): SessionWindowRole {
  const role = new URLSearchParams(window.location.search).get("helppyeWindow");
  return role === "ai" || role === "chat" ? role : "main";
}

function sessionUrl(role: Exclude<SessionWindowRole, "main">, startedAt: number): string {
  const url = new URL(window.location.href);
  url.searchParams.set("helppyeWindow", role);
  url.searchParams.set("startedAt", String(startedAt));
  return `${url.pathname}${url.search}${url.hash}`;
}

async function closeIfExists(label: string): Promise<void> {
  const win = await WebviewWindow.getByLabel(label);
  if (win) await win.close().catch(() => {});
}

export async function openSessionWindows(startedAt: number): Promise<boolean> {
  if (!isTauriRuntime()) return false;

  await Promise.all([closeIfExists(AI_WINDOW), closeIfExists(CHAT_WINDOW)]);

  const ai = new WebviewWindow(AI_WINDOW, {
    url: sessionUrl("ai", startedAt),
    title: "Helppye AI",
    width: 367,
    height: 188,
    minWidth: 320,
    minHeight: 150,
    x: 64,
    y: 64,
    decorations: false,
    transparent: true,
    alwaysOnTop: true,
    skipTaskbar: true,
    shadow: true,
    resizable: true,
    theme: "dark",
  });

  const chat = new WebviewWindow(CHAT_WINDOW, {
    url: sessionUrl("chat", startedAt),
    title: "Helppye Session",
    width: 367,
    height: 388,
    minWidth: 320,
    minHeight: 320,
    x: 64,
    y: 268,
    decorations: false,
    transparent: false,
    alwaysOnTop: true,
    skipTaskbar: true,
    shadow: true,
    resizable: true,
    theme: "dark",
  });

  await Promise.all([
    new Promise<void>((resolve) => {
      ai.once("tauri://created", () => resolve());
      ai.once("tauri://error", () => resolve());
    }),
    new Promise<void>((resolve) => {
      chat.once("tauri://created", () => resolve());
      chat.once("tauri://error", () => resolve());
    }),
  ]);

  return true;
}

export async function closeSessionWindows(): Promise<void> {
  if (!isTauriRuntime()) return;
  await Promise.all([closeIfExists(AI_WINDOW), closeIfExists(CHAT_WINDOW)]);
}

export function getWindowStartedAt(): number {
  const value = Number(new URLSearchParams(window.location.search).get("startedAt"));
  return Number.isFinite(value) && value > 0 ? value : Date.now();
}
