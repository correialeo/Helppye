import { useEffect } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";

export function useTransparentWindowBackground(enabled = true) {
  useEffect(() => {
    if (!enabled || !("__TAURI_INTERNALS__" in window)) return;
    getCurrentWindow().setBackgroundColor("#00000000").catch(() => {});
  }, [enabled]);
}
