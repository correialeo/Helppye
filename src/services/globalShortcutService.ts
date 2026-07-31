import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export const GLOBAL_SESSION_TOGGLE_EVENT = "helppye://global-session-toggle";

export function onGlobalSessionToggle(handler: () => void): Promise<UnlistenFn> {
  return listen(GLOBAL_SESSION_TOGGLE_EVENT, () => handler());
}
