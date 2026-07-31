import { type ButtonHTMLAttributes, forwardRef } from "react";
import { cx } from "../../utils/cx";

/** No border, no fill — "Voltar", "Pular", discrete secondary actions that should read
 * as quieter than SecondaryButton, one step above plain text. */
export const GhostButton = forwardRef<HTMLButtonElement, ButtonHTMLAttributes<HTMLButtonElement>>(
  ({ className, children, ...props }, ref) => (
    <button
      ref={ref}
      type="button"
      className={cx(
        "inline-flex items-center justify-center gap-1.5 rounded-[8px] px-3 py-2 text-sm font-semibold text-neutral-400 transition-colors duration-150",
        "hover:bg-white/5 hover:text-neutral-200",
        "disabled:cursor-not-allowed disabled:opacity-50",
        className,
      )}
      {...props}
    >
      {children}
    </button>
  ),
);
GhostButton.displayName = "GhostButton";
