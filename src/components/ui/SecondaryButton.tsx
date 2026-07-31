import { type ButtonHTMLAttributes, forwardRef } from "react";
import { cx } from "../../utils/cx";

interface SecondaryButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  fullWidth?: boolean;
}

/** A visible but non-dominant action — "Testar conexão", "Alterar modelo". Bordered
 * surface, not filled, so it never competes with a PrimaryButton on the same screen. */
export const SecondaryButton = forwardRef<HTMLButtonElement, SecondaryButtonProps>(
  ({ className, fullWidth, children, ...props }, ref) => (
    <button
      ref={ref}
      type="button"
      className={cx(
        "inline-flex items-center justify-center gap-2 rounded-[9px] border border-white/12 bg-white/[0.04] px-4 py-2.5 text-sm font-semibold text-neutral-100 transition-colors duration-150",
        "hover:border-white/25 hover:bg-surface-raised",
        "disabled:cursor-not-allowed disabled:opacity-50",
        fullWidth && "w-full",
        className,
      )}
      {...props}
    >
      {children}
    </button>
  ),
);
SecondaryButton.displayName = "SecondaryButton";
