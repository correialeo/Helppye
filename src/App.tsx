import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

type AudioSourceKind = "microphone" | "system_output";

interface AudioDevice {
  id: string;
  name: string;
  source: AudioSourceKind;
  is_default: boolean;
}

type AudioCaptureEvent =
  | { type: "started"; device: AudioDevice }
  | { type: "frame"; source: AudioSourceKind; samples: number[]; sample_rate: number; channels: number; timestamp_ms: number }
  | { type: "device_disconnected"; source: AudioSourceKind; device_id: string }
  | { type: "error"; source: AudioSourceKind; message: string }
  | { type: "stopped"; source: AudioSourceKind };

type TranscriptEvent =
  | {
      type: "ready";
      segment_id: number;
      source: AudioSourceKind;
      text: string;
      language: string | null;
      started_at: number;
      ended_at: number;
      processing_time_ms: number;
    }
  | { type: "failed"; segment_id: number; source: AudioSourceKind; message: string };

function rmsDbfs(samples: number[]): number {
  if (samples.length === 0) return -Infinity;
  const meanSquare = samples.reduce((sum, s) => sum + s * s, 0) / samples.length;
  const rms = Math.sqrt(meanSquare);
  return rms > 0 ? 20 * Math.log10(rms) : -Infinity;
}

interface PanelConfig {
  source: AudioSourceKind;
  title: string;
  subtitle: string;
  listDevicesCommand: string;
  startCommand: string;
  stopCommand: string;
}

const PANELS: PanelConfig[] = [
  {
    source: "microphone",
    title: "Microphone",
    subtitle: "cpal input capture",
    listDevicesCommand: "list_audio_devices_command",
    startCommand: "start_microphone_capture_command",
    stopCommand: "stop_microphone_capture_command",
  },
  {
    source: "system_output",
    title: "System output",
    subtitle: "WASAPI loopback capture (Windows only)",
    listDevicesCommand: "list_system_audio_devices_command",
    startCommand: "start_system_audio_capture_command",
    stopCommand: "stop_system_audio_capture_command",
  },
];

function CapturePanel({ config }: { config: PanelConfig }) {
  const [devices, setDevices] = useState<AudioDevice[]>([]);
  const [capturing, setCapturing] = useState(false);
  const [levelDb, setLevelDb] = useState(-Infinity);
  const [frameCount, setFrameCount] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [transcript, setTranscript] = useState("");
  const [transcriptError, setTranscriptError] = useState<string | null>(null);
  const levelDecayTimer = useRef<number | null>(null);

  const refreshDevices = useCallback(() => {
    invoke<AudioDevice[]>(config.listDevicesCommand)
      .then(setDevices)
      .catch((e) => setError(String(e)));
  }, [config.listDevicesCommand]);

  useEffect(() => {
    refreshDevices();
  }, [refreshDevices]);

  useEffect(() => {
    const unlisten = listen<AudioCaptureEvent>("audio://capture-event", (event) => {
      const payload = event.payload;
      // "started" doesn't carry `source` directly but does carry a device whose own
      // `source` field identifies which panel it belongs to.
      const eventSource = payload.type === "started" ? payload.device.source : payload.source;
      if (eventSource !== config.source) return;

      if (payload.type === "frame") {
        // Throttled by design: the level meter only updates on frame arrival (100ms
        // frames from CaptureConfig::default), and decays to silence if frames stop.
        setLevelDb(rmsDbfs(payload.samples));
        setFrameCount((n) => n + 1);
        if (levelDecayTimer.current !== null) window.clearTimeout(levelDecayTimer.current);
        levelDecayTimer.current = window.setTimeout(() => setLevelDb(-Infinity), 300);
      } else if (payload.type === "error") {
        setError(payload.message);
        setCapturing(false);
      } else if (payload.type === "device_disconnected") {
        setError(`device disconnected: ${payload.device_id}`);
        setCapturing(false);
      } else if (payload.type === "stopped") {
        setCapturing(false);
      }
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [config.source]);

  useEffect(() => {
    const unlisten = listen<TranscriptEvent>("transcription://event", (event) => {
      const payload = event.payload;
      if (payload.source !== config.source) return;

      if (payload.type === "ready") {
        setTranscriptError(null);
        if (payload.text.length > 0) {
          setTranscript((t) => (t.length > 0 ? `${t} ${payload.text}` : payload.text));
        }
      } else {
        setTranscriptError(payload.message);
      }
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [config.source]);

  const startCapture = async () => {
    setError(null);
    setFrameCount(0);
    try {
      await invoke(config.startCommand);
      setCapturing(true);
    } catch (e) {
      setError(String(e));
    }
  };

  const stopCapture = async () => {
    try {
      await invoke(config.stopCommand);
    } catch (e) {
      setError(String(e));
    } finally {
      setCapturing(false);
    }
  };

  const levelPercent = Number.isFinite(levelDb) ? Math.min(100, Math.max(0, (levelDb + 60) * (100 / 60))) : 0;

  return (
    <section className="flex w-full max-w-xs flex-col items-center gap-3 rounded-lg border border-neutral-800 p-4">
      <div className="text-center">
        <h2 className="text-base font-semibold">{config.title}</h2>
        <p className="text-xs text-neutral-500">{config.subtitle}</p>
      </div>

      <div className="w-full text-left text-xs text-neutral-400">
        <p className="mb-1 font-medium text-neutral-300">Devices ({devices.length})</p>
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
        className="w-full rounded bg-indigo-600 px-4 py-2 text-sm font-medium hover:bg-indigo-500"
      >
        {capturing ? "Stop capture" : "Start capture"}
      </button>

      <div className="w-full">
        <div className="h-3 w-full overflow-hidden rounded bg-neutral-800">
          <div className="h-full bg-emerald-500 transition-all" style={{ width: `${levelPercent}%` }} />
        </div>
        <p className="mt-1 text-xs text-neutral-500">
          {capturing ? `${frameCount} frames` : "idle"} — {Number.isFinite(levelDb) ? `${levelDb.toFixed(1)} dBFS` : "-∞ dBFS"}
        </p>
      </div>

      {error && <p className="text-xs text-red-400">{error}</p>}

      <div className="w-full text-left">
        <p className="mb-1 text-xs font-medium text-neutral-300">Transcrição</p>
        <p className="max-h-32 min-h-12 overflow-y-auto rounded border border-neutral-700 p-2 text-xs text-neutral-200">
          {transcript || <span className="text-neutral-500">Aguardando fala...</span>}
        </p>
        {transcriptError && <p className="mt-1 text-xs text-red-400">{transcriptError}</p>}
      </div>
    </section>
  );
}

type LanguageChoice = "pt" | "auto";

function modelNameFromPath(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

function ModelConfigPanel() {
  const [modelPath, setModelPath] = useState("");
  const [language, setLanguage] = useState<LanguageChoice>("pt");
  const [status, setStatus] = useState<"idle" | "loading" | "loaded" | "error">("idle");
  const [error, setError] = useState<string | null>(null);

  const loadModel = async () => {
    setStatus("loading");
    setError(null);
    try {
      await invoke("configure_transcription_command", {
        modelPath,
        modelName: modelNameFromPath(modelPath),
        language: language === "auto" ? null : language,
      });
      setStatus("loaded");
    } catch (e) {
      setStatus("error");
      setError(String(e));
    }
  };

  return (
    <section className="flex w-full max-w-md flex-col gap-2 rounded-lg border border-neutral-800 p-4 text-left">
      <h2 className="text-base font-semibold">Modelo de transcrição</h2>
      <label className="text-xs text-neutral-400">
        Caminho do modelo (.bin, formato ggml/whisper.cpp)
        <input
          type="text"
          value={modelPath}
          onChange={(e) => setModelPath(e.target.value)}
          placeholder="/caminho/para/ggml-base.bin"
          className="mt-1 w-full rounded border border-neutral-700 bg-neutral-900 px-2 py-1 text-sm text-neutral-200"
        />
      </label>

      <fieldset className="flex gap-4 text-xs text-neutral-400">
        <label className="flex items-center gap-1">
          <input
            type="radio"
            name="transcription-language"
            checked={language === "pt"}
            onChange={() => setLanguage("pt")}
          />
          Português
        </label>
        <label className="flex items-center gap-1">
          <input
            type="radio"
            name="transcription-language"
            checked={language === "auto"}
            onChange={() => setLanguage("auto")}
          />
          Automático
        </label>
      </fieldset>

      <button
        type="button"
        onClick={loadModel}
        disabled={modelPath.length === 0 || status === "loading"}
        className="w-full rounded bg-indigo-600 px-4 py-2 text-sm font-medium hover:bg-indigo-500 disabled:opacity-50"
      >
        {status === "loading" ? "Carregando..." : "Carregar modelo"}
      </button>

      {status === "loaded" && <p className="text-xs text-emerald-400">Modelo carregado.</p>}
      {error && <p className="text-xs text-red-400">{error}</p>}
    </section>
  );
}

export default function App() {
  return (
    <main className="flex min-h-screen flex-col items-center justify-center gap-4 p-6 text-center">
      <h1 className="text-2xl font-semibold">Helppye</h1>
      <p className="text-sm text-neutral-400">Microphone + system audio capture test</p>

      <ModelConfigPanel />

      <div className="flex flex-col gap-4 sm:flex-row">
        {PANELS.map((config) => (
          <CapturePanel key={config.source} config={config} />
        ))}
      </div>
    </main>
  );
}
