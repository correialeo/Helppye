import { useEffect } from "react";

interface ShortcutHandlers {
  /** Ctrl/Cmd+D — start or end the session. */
  onToggleSession?: () => void;
  /** Ctrl/Cmd+Enter — open settings. */
  onOpenSettings?: () => void;
  /** Ctrl/Cmd+Shift+Enter — generate a suggestion manually. */
  onRegenerate?: () => void;
}

/** App-wide shortcuts (see docs/shortcuts.md). All three are `mod`-combos, which is why
 * they're safe to handle globally even while a text field has focus — nothing here
 * collides with normal typing the way a bare `Enter` binding would. */
export function useKeyboardShortcuts({ onToggleSession, onOpenSettings, onRegenerate }: ShortcutHandlers) {
  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      const mod = event.metaKey || event.ctrlKey;
      if (!mod) return;

      if (event.key.toLowerCase() === "d" && !event.shiftKey) {
        if (!onToggleSession) return;
        event.preventDefault();
        onToggleSession();
        return;
      }

      if (event.key === "Enter" && event.shiftKey) {
        if (!onRegenerate) return;
        event.preventDefault();
        onRegenerate();
        return;
      }

      if (event.key === "Enter" && !event.shiftKey) {
        if (!onOpenSettings) return;
        event.preventDefault();
        onOpenSettings();
      }
    };

    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [onToggleSession, onOpenSettings, onRegenerate]);
}
