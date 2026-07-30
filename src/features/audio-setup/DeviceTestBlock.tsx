import type { ReactNode } from "react";
import { DeviceOption } from "../../components/ui/DeviceOption";
import { Select } from "../../components/ui/Select";
import { useAudioCapture } from "../../hooks/useAudioCapture";
import type { AudioSourceKind } from "../../types/audio";
import type { CaptureStatus } from "../../stores/useAudioCaptureStore";

function statusLabel(status: CaptureStatus, levelDb: number): string {
  switch (status.kind) {
    case "capturing":
      return Number.isFinite(levelDb) ? "Ouvindo" : "Sem sinal";
    case "switching":
      return "Trocando dispositivo...";
    case "disconnected":
      return "Dispositivo desconectado";
    case "error":
      return "Sem permissão";
    case "idle":
      return "Parado";
  }
}

/** One live device row — icon, name, level meter, status, and (when more than one
 * device exists) a picker. Shared by the audio-setup onboarding step and the settings
 * screen's "Áudio" section, since it's the exact same live state either place. */
export function DeviceTestBlock({ icon, title, source }: { icon: ReactNode; title: string; source: AudioSourceKind }) {
  const { status, levelDb, levelPercent, devices, selectedId, start, selectDevice } = useAudioCapture(source);
  const selected = devices.find((d) => d.id === selectedId);

  return (
    <DeviceOption
      icon={icon}
      title={title}
      deviceName={selected?.name ?? (status.kind === "idle" ? "Nenhum dispositivo iniciado" : "Padrão do sistema")}
      levelPercent={status.kind === "capturing" ? levelPercent : 0}
      statusLabel={status.kind === "idle" ? "Toque para ouvir" : statusLabel(status, levelDb)}
      picker={
        devices.length > 1 ? (
          <div className="w-36">
            <Select
              value={selectedId}
              onChange={selectDevice}
              options={devices.map((d) => ({
                value: d.id,
                label: d.name,
                detail: d.is_default ? "Padrão do sistema" : undefined,
              }))}
            />
          </div>
        ) : status.kind === "idle" ? (
          <button type="button" onClick={start} className="text-xs font-medium text-brand-400 hover:text-brand-300">
            Iniciar
          </button>
        ) : undefined
      }
    />
  );
}
