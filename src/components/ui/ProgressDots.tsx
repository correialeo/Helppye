import { cx } from "../../utils/cx";

interface ProgressDotsProps {
  total: number;
  /** 0-indexed current step. */
  current: number;
  className?: string;
}

/** Discrete onboarding progress — deliberately not "Etapa 4 de 8": a row of dots reads at
 * a glance without making the flow feel long. See docs/onboarding.md. */
export function ProgressDots({ total, current, className }: ProgressDotsProps) {
  return (
    <div className={cx("flex items-center gap-1.5", className)} role="progressbar" aria-valuenow={current + 1} aria-valuemin={1} aria-valuemax={total}>
      {Array.from({ length: total }, (_, index) => (
        <span
          key={index}
          className={cx(
            "h-1.5 rounded-full transition-all duration-200",
            index === current ? "w-4 bg-brand-400" : index < current ? "w-1.5 bg-brand-400/50" : "w-1.5 bg-white/12",
          )}
        />
      ))}
    </div>
  );
}
