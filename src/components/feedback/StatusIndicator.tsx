import { cx } from "../../utils/cx";

export type StatusTone = "neutral" | "active" | "warning" | "error";

const DOT_CLASSES: Record<StatusTone, string> = {
  neutral: "bg-neutral-500",
  active: "bg-emerald-400",
  warning: "bg-amber-400",
  error: "bg-red-400",
};

interface StatusIndicatorProps {
  label: string;
  tone?: StatusTone;
  /** Subtle pulse for "live" states (listening, capturing) — off by default so it's used
   * deliberately, not on every dot. */
  pulse?: boolean;
  className?: string;
}

/** A dot + label — the app's one recurring way to say "here's the current state" without
 * a word of jargon. Used for capture status, provider connection, session state. */
export function StatusIndicator({ label, tone = "neutral", pulse, className }: StatusIndicatorProps) {
  return (
    <span className={cx("inline-flex items-center gap-1.5 text-xs text-neutral-400", className)}>
      <span className={cx("h-1.5 w-1.5 rounded-full", DOT_CLASSES[tone], pulse && "animate-pulse-soft")} aria-hidden="true" />
      {label}
    </span>
  );
}
