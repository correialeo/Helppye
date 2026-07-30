import { cx } from "../../utils/cx";

interface ProgressBarProps {
  percent: number;
  className?: string;
}

/** Determinate progress — model download, nothing else. Solid fill, same reasoning as
 * AudioLevelMeter (gradients are reserved for a short, deliberate list of elements). */
export function ProgressBar({ percent, className }: ProgressBarProps) {
  const clamped = Math.min(100, Math.max(0, percent));
  return (
    <div
      role="progressbar"
      aria-valuenow={Math.round(clamped)}
      aria-valuemin={0}
      aria-valuemax={100}
      className={cx("h-2 w-full overflow-hidden rounded-full bg-white/8", className)}
    >
      <div className="h-full rounded-full bg-brand-500 transition-[width] duration-200 ease-out" style={{ width: `${clamped}%` }} />
    </div>
  );
}
