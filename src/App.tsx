import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

interface AudioDevice {
  id: string;
  name: string;
  source: "microphone" | "system_output";
  is_default: boolean;
}

type AudioCaptureEvent =
  | { type: "started"; device: AudioDevice }
  | { type: "frame"; source: string; samples: number[]; sample_rate: number; channels: number; timestamp_ms: number }
  | { type: "device_disconnected"; device_id: string }
  | { type: "error"; message: string }
  | { type: "stopped" };

function rmsDbfs(samples: number[]): number {
  if (samples.length === 0) return -Infinity;
  const meanSquare = samples.reduce((sum, s) => sum + s * s, 0) / samples.length;
  const rms = Math.sqrt(meanSquare);
  return rms > 0 ? 20 * Math.log10(rms) : -Infinity;
}

export default function App() {
  const [devices, setDevices] = useState<AudioDevice[]>([]);
  const [capturing, setCapturing] = useState(false);
  const [levelDb, setLevelDb] = useState(-Infinity);
  const [frameCount, setFrameCount] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const levelDecayTimer = useRef<number | null>(null);

  const refreshDevices = useCallback(() => {
    invoke<AudioDevice[]>("list_audio_devices_command")
      .then(setDevices)
      .catch((e) => setError(String(e)));
  }, []);

  useEffect(() => {
    refreshDevices();
  }, [refreshDevices]);

  useEffect(() => {
    const unlisten = listen<AudioCaptureEvent>("audio://capture-event", (event) => {
      const payload = event.payload;
      if (payload.type === "frame") {
        setLevelDb(rmsDbfs(payload.samples));
        setFrameCount((n) => n + 1);
        if (levelDecayTimer.current !== null) window.clearTimeout(levelDecayTimer.current);
        levelDecayTimer.current = window.setTimeout(() => setLevelDb(-Infinity), 300);
      } else if (payload.type === "error") {
        setError(payload.message);
      } else if (payload.type === "stopped") {
        setCapturing(false);
      }
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  const startCapture = async () => {
    setError(null);
    setFrameCount(0);
    try {
      await invoke("start_microphone_capture_command");
      setCapturing(true);
    } catch (e) {
      setError(String(e));
    }
  };

  const stopCapture = async () => {
    try {
      await invoke("stop_capture_command");
    } catch (e) {
      setError(String(e));
    } finally {
      setCapturing(false);
    }
  };

  const levelPercent = Number.isFinite(levelDb) ? Math.min(100, Math.max(0, (levelDb + 60) * (100 / 60))) : 0;

  return (
    <main className="flex h-screen flex-col items-center justify-center gap-4 p-6 text-center">
      <h1 className="text-2xl font-semibold">Helppye</h1>
      <p className="text-sm text-neutral-400">Microphone capture test</p>

      <div className="w-full max-w-xs text-left text-xs text-neutral-400">
        <p className="mb-1 font-medium text-neutral-300">Input devices ({devices.length})</p>
        <ul className="max-h-24 overflow-y-auto rounded border border-neutral-700 p-2">
          {devices.map((d) => (
            <li key={d.id}>
              {d.name}
              {d.is_default ? " (default)" : ""}
            </li>
          ))}
          {devices.length === 0 && <li className="text-neutral-500">No devices found</li>}
        </ul>
      </div>

      <button
        type="button"
        onClick={capturing ? stopCapture : startCapture}
        className="rounded bg-indigo-600 px-4 py-2 text-sm font-medium hover:bg-indigo-500"
      >
        {capturing ? "Stop capture" : "Start capture"}
      </button>

      <div className="w-full max-w-xs">
        <div className="h-3 w-full overflow-hidden rounded bg-neutral-800">
          <div className="h-full bg-emerald-500 transition-all" style={{ width: `${levelPercent}%` }} />
        </div>
        <p className="mt-1 text-xs text-neutral-500">
          {capturing ? `${frameCount} frames` : "idle"} — {Number.isFinite(levelDb) ? `${levelDb.toFixed(1)} dBFS` : "-∞ dBFS"}
        </p>
      </div>

      {error && <p className="max-w-xs text-xs text-red-400">{error}</p>}
    </main>
  );
}
