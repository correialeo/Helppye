import { useEffect } from "react";
import { getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";
import type { AppScreen } from "../app/appFlow";

const APP_SIZE = new LogicalSize(420, 760);
const SESSION_SIZE = new LogicalSize(380, 620);
const SESSION_MIN_SIZE = new LogicalSize(320, 420);
const APP_MIN_SIZE = new LogicalSize(360, 560);

/**
 * The session window is meant to feel more compact than the rest of the app (see
 * docs/design-system.md §Janela de sessão) — resized here via the existing
 * `@tauri-apps/api/window` JS API (no Rust changes: `core:window:allow-set-size`/
 * `allow-set-min-size` were added to `capabilities/default.json` since the default
 * capability set only grants read-only window queries). Best-effort: a resize failure
 * (e.g. running outside Tauri, in a plain browser during `npm run dev` without the
 * Tauri shell) never blocks the screen from rendering.
 */
export function useWindowMode(screen: AppScreen) {
  useEffect(() => {
    const win = getCurrentWindow();
    const isSession = screen === "session";
    win.setMinSize(isSession ? SESSION_MIN_SIZE : APP_MIN_SIZE).catch(() => {});
    win.setSize(isSession ? SESSION_SIZE : APP_SIZE).catch(() => {});
  }, [screen]);
}
