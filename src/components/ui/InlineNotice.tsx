import type { ReactNode } from "react";
import { AlertTriangle, CheckCircle2, Info, XCircle } from "lucide-react";
import { cx } from "../../utils/cx";

type NoticeTone = "info" | "success" | "warning" | "error";

const TONE_STYLES: Record<NoticeTone, { icon: typeof Info; classes: string; iconClass: string }> = {
  info: {
    icon: Info,
    classes: "border-white/12 bg-white/5 text-neutral-300",
    iconClass: "text-neutral-400",
  },
  success: {
    icon: CheckCircle2,
    classes: "border-emerald-500/25 bg-emerald-500/8 text-emerald-300",
    iconClass: "text-emerald-400",
  },
  warning: {
    icon: AlertTriangle,
    classes: "border-amber-500/25 bg-amber-500/8 text-amber-300",
    iconClass: "text-amber-400",
  },
  error: {
    icon: XCircle,
    classes: "border-red-500/25 bg-red-500/8 text-red-300",
    iconClass: "text-red-400",
  },
};

interface InlineNoticeProps {
  tone?: NoticeTone;
  children: ReactNode;
  action?: ReactNode;
  className?: string;
}

/** A quiet banner, never a modal, for things the user should notice without being
 * interrupted — semantic colors used discretely (low-alpha fills), never as a loud
 * saturated block. See docs/design-system.md §Paleta. */
export function InlineNotice({ tone = "info", children, action, className }: InlineNoticeProps) {
  const { icon: Icon, classes, iconClass } = TONE_STYLES[tone];
  return (
    <div className={cx("flex items-start gap-2.5 rounded-lg border px-3 py-2.5 text-sm", classes, className)}>
      <Icon className={cx("mt-0.5 h-4 w-4 flex-shrink-0", iconClass)} aria-hidden="true" />
      <div className="flex-1 leading-relaxed">{children}</div>
      {action}
    </div>
  );
}
