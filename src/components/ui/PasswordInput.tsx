import { type InputHTMLAttributes, forwardRef, useId, useState } from "react";
import { Eye, EyeOff } from "lucide-react";
import { cx } from "../../utils/cx";

interface PasswordInputProps extends Omit<InputHTMLAttributes<HTMLInputElement>, "type"> {
  label?: string;
}

/** Masked by default (API keys), with a reveal toggle — never renders the value into the
 * DOM as plain text unless the user explicitly asks to see it, and never logs/persists
 * it (that's the caller's job: keys only ever go to the OS keychain, see
 * docs/design-system.md §Segurança). */
export const PasswordInput = forwardRef<HTMLInputElement, PasswordInputProps>(
  ({ className, label, id, ...props }, ref) => {
    const generatedId = useId();
    const inputId = id ?? generatedId;
    const [revealed, setRevealed] = useState(false);

    return (
      <div className="flex flex-col gap-1.5">
        {label && (
          <label htmlFor={inputId} className="text-xs font-medium text-neutral-400">
            {label}
          </label>
        )}
        <div className="relative">
          <input
            ref={ref}
            id={inputId}
            type={revealed ? "text" : "password"}
            className={cx(
              "w-full rounded-lg border border-white/12 bg-surface px-3 py-2.5 pr-10 text-sm text-neutral-100 placeholder:text-neutral-500 transition-colors duration-150",
              "hover:border-white/20 focus:border-brand-400/70",
              "disabled:cursor-not-allowed disabled:opacity-50",
              className,
            )}
            {...props}
          />
          <button
            type="button"
            onClick={() => setRevealed((v) => !v)}
            aria-label={revealed ? "Ocultar valor" : "Mostrar valor"}
            className="absolute inset-y-0 right-0 flex w-9 items-center justify-center text-neutral-500 transition-colors hover:text-neutral-200"
          >
            {revealed ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}
          </button>
        </div>
      </div>
    );
  },
);
PasswordInput.displayName = "PasswordInput";
