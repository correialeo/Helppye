import { WebviewWindow } from "@tauri-apps/api/webviewWindow";

export type AppWindowRole = "main" | "ai" | "chat" | "settings";

const AI_WINDOW = "helppye-ai-response";
const CHAT_WINDOW = "helppye-session-chat";
const SETTINGS_WINDOW = "helppye-settings";

function isTauriRuntime(): boolean {
  return "__TAURI_INTERNALS__" in window;
}

export function getSessionWindowRole(): AppWindowRole {
  const role = new URLSearchParams(window.location.search).get("helppyeWindow");
  return role === "ai" || role === "chat" || role === "settings" ? role : "main";
}

function appUrl(role: Exclude<AppWindowRole, "main">, startedAt?: number): string {
  const url = new URL(window.location.href);
  url.searchParams.set("helppyeWindow", role);
  if (startedAt) url.searchParams.set("startedAt", String(startedAt));
  return `${url.pathname}${url.search}${url.hash}`;
}

async function getWindow(label: string): Promise<WebviewWindow | null> {
  return WebviewWindow.getByLabel(label);
}

async function closeIfExists(label: string): Promise<void> {
  const win = await getWindow(label);
  if (win) await win.close().catch(() => {});
}

async function showAndFocus(label: string): Promise<boolean> {
  const win = await getWindow(label);
  if (!win) return false;
  await win.unminimize().catch(() => {});
  await win.show().catch(() => {});
  await win.setFocus().catch(() => {});
  return true;
}

export async function openSessionWindows(startedAt: number): Promise<boolean> {
  if (!isTauriRuntime()) return false;

  await Promise.all([closeIfExists(AI_WINDOW), closeIfExists(CHAT_WINDOW)]);

  const ai = new WebviewWindow(AI_WINDOW, {
    url: appUrl("ai", startedAt),
    title: "Helppye AI",
    width: 500,
    height: 210,
    minWidth: 400,
    minHeight: 160,
    x: 64,
    y: 64,
    decorations: false,
    transparent: true,
    backgroundColor: "#00000000",
    alwaysOnTop: true,
    skipTaskbar: true,
    shadow: false,
    resizable: true,
    theme: "dark",
  });

  const chat = new WebviewWindow(CHAT_WINDOW, {
    url: appUrl("chat", startedAt),
    title: "Helppye Session",
    width: 500,
    height: 460,
    minWidth: 400,
    minHeight: 340,
    x: 64,
    y: 282,
    decorations: false,
    transparent: true,
    backgroundColor: "#00000000",
    alwaysOnTop: true,
    skipTaskbar: true,
    shadow: false,
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

export async function restoreSessionWindows(): Promise<boolean> {
  if (!isTauriRuntime()) return false;
  const [ai, chat] = await Promise.all([showAndFocus(AI_WINDOW), showAndFocus(CHAT_WINDOW)]);
  return ai || chat;
}

export async function closeSessionWindows(): Promise<void> {
  if (!isTauriRuntime()) return;
  await Promise.all([closeIfExists(AI_WINDOW), closeIfExists(CHAT_WINDOW)]);
}

export async function openSettingsWindow(): Promise<boolean> {
  if (!isTauriRuntime()) return false;

  if (await showAndFocus(SETTINGS_WINDOW)) return true;

  const settings = new WebviewWindow(SETTINGS_WINDOW, {
    url: appUrl("settings"),
    title: "Helppye Settings",
    width: 820,
    height: 760,
    minWidth: 520,
    minHeight: 560,
    center: true,
    decorations: true,
    transparent: false,
    alwaysOnTop: false,
    skipTaskbar: false,
    shadow: true,
    resizable: true,
    theme: "dark",
  });

  await new Promise<void>((resolve) => {
    settings.once("tauri://created", () => resolve());
    settings.once("tauri://error", () => resolve());
  });

  return true;
}

export function getWindowStartedAt(): number {
  const value = Number(new URLSearchParams(window.location.search).get("startedAt"));
  return Number.isFinite(value) && value > 0 ? value : Date.now();
}
