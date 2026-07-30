import { cx } from "../../utils/cx";

interface ToggleProps {
  checked: boolean;
  onChange: (checked: boolean) => void;
  label: string;
  description?: string;
}

/** A real `role="switch"` — the only place in the app besides Dialog where accessible
 * semantics matter more than a plain styled `<button>`. */
export function Toggle({ checked, onChange, label, description }: ToggleProps) {
  return (
    <div className="flex items-center justify-between gap-3 py-1">
      <div>
        <p className="text-sm text-neutral-200">{label}</p>
        {description && <p className="text-xs text-neutral-500">{description}</p>}
      </div>
      <button
        type="button"
        role="switch"
        aria-checked={checked}
        aria-label={label}
        onClick={() => onChange(!checked)}
        className={cx(
          "relative h-6 w-10 flex-shrink-0 rounded-full transition-colors duration-150",
          checked ? "bg-brand-500" : "bg-white/12",
        )}
      >
        <span
          className={cx(
            "absolute top-0.5 h-5 w-5 rounded-full bg-white shadow-soft transition-transform duration-150",
            checked ? "translate-x-[18px]" : "translate-x-0.5",
          )}
        />
      </button>
    </div>
  );
}
