import { type ButtonHTMLAttributes, forwardRef } from "react";
import { cx } from "../../utils/cx";

interface IconButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  /** Required, not optional — an icon-only control is invisible to screen readers and to
   * anyone scanning by keyboard without it. See docs/design-system.md §Acessibilidade. */
  "aria-label": string;
  active?: boolean;
}

export const IconButton = forwardRef<HTMLButtonElement, IconButtonProps>(
  ({ className, active, children, ...props }, ref) => (
    <button
      ref={ref}
      type="button"
      className={cx(
        "inline-flex h-8 w-8 items-center justify-center rounded-md text-neutral-400 transition-colors duration-150",
        "hover:bg-white/8 hover:text-neutral-100",
        active && "bg-white/8 text-neutral-100",
        "disabled:cursor-not-allowed disabled:opacity-40",
        className,
      )}
      {...props}
    >
      {children}
    </button>
  ),
);
IconButton.displayName = "IconButton";
