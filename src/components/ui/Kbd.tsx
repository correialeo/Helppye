import { cx } from "../../utils/cx";

/** Renders a shortcut as key-like chips, e.g. <Kbd keys={["mod", "D"]} /> → "⌘ D" on
 * macOS, "Ctrl D" elsewhere. "mod" is the one platform-dependent token; everything else
 * is shown verbatim. See hooks/useKeyboardShortcuts.ts for the matching logic. */
const isMac =
  typeof navigator !== "undefined" && /Mac|iPhone|iPod|iPad/.test(navigator.platform ?? navigator.userAgent);

export function modKeyLabel(): string {
  return isMac ? "⌘" : "Ctrl";
}

function keyLabel(key: string): string {
  if (key === "mod") return modKeyLabel();
  if (key === "shift") return isMac ? "⇧" : "Shift";
  if (key === "enter") return isMac ? "⏎" : "Enter";
  return key.toUpperCase();
}

export function Kbd({ keys, className }: { keys: string[]; className?: string }) {
  return (
    <span className={cx("inline-flex items-center gap-1", className)}>
      {keys.map((key, index) => (
        <kbd
          key={index}
          className="rounded border border-white/15 bg-white/5 px-1.5 py-0.5 font-mono text-[10px] leading-none text-neutral-400"
        >
          {keyLabel(key)}
        </kbd>
      ))}
    </span>
  );
}
