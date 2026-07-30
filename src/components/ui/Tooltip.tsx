import { type ReactElement, cloneElement, useId, useState } from "react";
import { cx } from "../../utils/cx";

interface TooltipProps {
  label: string;
  children: ReactElement<Record<string, unknown>>;
  side?: "top" | "bottom";
}

/** Wraps a single focusable child, showing `label` on hover *and* keyboard focus (a
 * hover-only tooltip is invisible to keyboard users) via `aria-describedby`, never as the
 * only way a piece of information is conveyed. */
export function Tooltip({ label, children, side = "top" }: TooltipProps) {
  const [visible, setVisible] = useState(false);
  const id = useId();

  return (
    <span
      className="relative inline-flex"
      onMouseEnter={() => setVisible(true)}
      onMouseLeave={() => setVisible(false)}
      onFocus={() => setVisible(true)}
      onBlur={() => setVisible(false)}
    >
      {cloneElement(children, { "aria-describedby": id })}
      <span
        role="tooltip"
        id={id}
        className={cx(
          "pointer-events-none absolute left-1/2 z-30 -translate-x-1/2 whitespace-nowrap rounded-md border border-white/10 bg-surface-raised px-2 py-1 text-xs text-neutral-200 shadow-soft transition-opacity duration-150",
          side === "top" ? "bottom-full mb-1.5" : "top-full mt-1.5",
          visible ? "opacity-100" : "opacity-0",
        )}
      >
        {label}
      </span>
    </span>
  );
}
