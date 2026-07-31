import { type InputHTMLAttributes, forwardRef, useId } from "react";
import { cx } from "../../utils/cx";

interface TextInputProps extends InputHTMLAttributes<HTMLInputElement> {
  label?: string;
  hint?: string;
}

/** Plain labeled text field. `label` renders a real `<label htmlFor>` — never a
 * placeholder standing in for a label, which fails the "understand in under three
 * seconds" and keyboard/screen-reader bars alike. */
export const TextInput = forwardRef<HTMLInputElement, TextInputProps>(
  ({ className, label, hint, id, ...props }, ref) => {
    const generatedId = useId();
    const inputId = id ?? generatedId;
    return (
      <div className="flex flex-col gap-1.5">
        {label && (
          <label htmlFor={inputId} className="text-xs font-medium text-neutral-400">
            {label}
          </label>
        )}
        <input
          ref={ref}
          id={inputId}
          className={cx(
            "w-full rounded-[4px] border border-white/14 bg-[#171717] px-3 py-2.5 text-sm text-neutral-100 placeholder:text-neutral-500 transition-colors duration-150",
            "hover:border-white/20 focus:border-brand-400/70",
            "disabled:cursor-not-allowed disabled:opacity-50",
            className,
          )}
          {...props}
        />
        {hint && <p className="text-xs text-neutral-500">{hint}</p>}
      </div>
    );
  },
);
TextInput.displayName = "TextInput";
