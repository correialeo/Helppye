import type { ReactNode } from "react";
import { AudioLevelMeter } from "../feedback/AudioLevelMeter";

interface DeviceOptionProps {
  icon: ReactNode;
  title: string;
  deviceName: string;
  levelPercent: number;
  statusLabel: string;
  picker?: ReactNode;
}

/** One compact block per audio source in the guided test — icon, device name, a live
 * level bar, and a one-line status. No device IDs, no sample rates, no channel counts —
 * see docs/onboarding.md §Áudio for what's deliberately left out of this view. */
export function DeviceOption({ icon, title, deviceName, levelPercent, statusLabel, picker }: DeviceOptionProps) {
  return (
    <div className="flex flex-col gap-2.5 rounded-lg border border-white/10 bg-surface px-4 py-3.5">
      <div className="flex items-center gap-2.5">
        <span className="flex h-8 w-8 flex-shrink-0 items-center justify-center rounded-md bg-white/6 text-neutral-300">
          {icon}
        </span>
        <div className="min-w-0 flex-1">
          <p className="text-sm font-medium text-neutral-100">{title}</p>
          <p className="truncate text-xs text-neutral-500">{deviceName}</p>
        </div>
      </div>
      <AudioLevelMeter percent={levelPercent} />
      <div className="flex items-center justify-between">
        <p className="text-xs text-neutral-500">{statusLabel}</p>
        {picker}
      </div>
    </div>
  );
}
