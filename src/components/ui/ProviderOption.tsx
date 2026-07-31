import type { ReactNode } from "react";
import { Check } from "lucide-react";
import { cx } from "../../utils/cx";

interface ProviderOptionProps {
  name: string;
  description: string;
  badge?: string;
  status?: ReactNode;
  selected?: boolean;
  onSelect: () => void;
}

/** A provider card — name, one-line description, a small badge ("Recomendado"), nothing
 * about endpoints/streaming/models. Selecting a provider is a decision about *character*
 * (local vs. cloud, fast vs. natural), not infrastructure — see docs/onboarding.md. */
export function ProviderOption({ name, description, badge, status, selected, onSelect }: ProviderOptionProps) {
  return (
    <button
      type="button"
      onClick={onSelect}
      aria-pressed={selected}
      className={cx(
        "flex w-full items-start justify-between gap-3 rounded-[8px] border px-4 py-3 text-left transition-colors duration-150",
        selected ? "border-brand-400/70 bg-brand-500/10" : "border-white/10 bg-[#111112] hover:border-white/20",
      )}
    >
      <div className="flex flex-col gap-0.5">
        <span className="flex items-center gap-2 text-sm font-medium text-neutral-100">
          {name}
          {badge && (
              <span className="rounded-full bg-white/10 px-1.5 py-0.5 text-[10px] font-bold uppercase tracking-wide text-white/62">
              {badge}
            </span>
          )}
        </span>
        <span className="text-xs text-neutral-400">{description}</span>
        {status}
      </div>
      <span
        className={cx(
          "mt-0.5 flex h-5 w-5 flex-shrink-0 items-center justify-center rounded-full border transition-colors duration-150",
          selected ? "border-brand-400 bg-brand-500 text-white" : "border-white/20 text-transparent",
        )}
        aria-hidden="true"
      >
        <Check className="h-3 w-3" />
      </span>
    </button>
  );
}
