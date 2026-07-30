import { cx } from "../../utils/cx";

interface AudioLevelMeterProps {
  /** 0–100. Callers convert from dBFS — see utils/audio.ts `dbfsToPercent`. */
  percent: number;
  className?: string;
}

/** A calm, solid-fill bar — no gradient here (see docs/design-system.md §Gradientes: the
 * palette reserves gradients for the primary button, background glow, generation state,
 * and identity details only). Width transitions smoothly; the level itself already
 * carries all the "liveliness" this needs. */
export function AudioLevelMeter({ percent, className }: AudioLevelMeterProps) {
  const clamped = Math.min(100, Math.max(0, percent));
  return (
    <div
      role="meter"
      aria-valuenow={Math.round(clamped)}
      aria-valuemin={0}
      aria-valuemax={100}
      className={cx("h-1.5 w-full overflow-hidden rounded-full bg-white/8", className)}
    >
      <div
        className="h-full rounded-full bg-brand-400 transition-[width] duration-150 ease-out"
        style={{ width: `${clamped}%` }}
      />
    </div>
  );
}
