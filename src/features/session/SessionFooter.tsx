import { useEffect, useState } from "react";
import { StatusIndicator, type StatusTone } from "../../components/feedback/StatusIndicator";
import { formatDuration } from "../../utils/format";
import type { CaptureStatus } from "../../stores/useAudioCaptureStore";

function toneFor(status: CaptureStatus): StatusTone {
  if (status.kind === "capturing") return "active";
  if (status.kind === "error" || status.kind === "disconnected") return "error";
  if (status.kind === "switching") return "warning";
  return "neutral";
}

interface SessionFooterProps {
  microphoneStatus: CaptureStatus;
  systemStatus: CaptureStatus;
  startedAt: number;
}

/** Infrastructure lives here, in the smallest possible form — two status dots and a
 * timer, never IDs, never sample rates. See docs/design-system.md §Complexidade
 * ocultada for the full list of what moved out of the main session view entirely. */
export function SessionFooter({ microphoneStatus, systemStatus, startedAt }: SessionFooterProps) {
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    const interval = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(interval);
  }, []);

  return (
    <footer className="flex items-center justify-between border-t border-white/8 px-4 py-2.5">
      <div className="flex items-center gap-3">
        <StatusIndicator label="Mic" tone={toneFor(microphoneStatus)} pulse={microphoneStatus.kind === "capturing"} />
        <StatusIndicator label="Sistema" tone={toneFor(systemStatus)} pulse={systemStatus.kind === "capturing"} />
      </div>
      <p className="font-mono text-xs text-neutral-500">{formatDuration(now - startedAt)}</p>
    </footer>
  );
}
