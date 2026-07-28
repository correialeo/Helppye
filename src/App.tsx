import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

type ModelInstallState =
  | { state: "not_installed" }
  | { state: "checking" }
  | { state: "downloading" }
  | { state: "cancelled" }
  | { state: "verifying" }
  | { state: "installing" }
  | { state: "ready" }
  | { state: "corrupted"; reason: string }
  | { state: "failed"; reason: string };

interface ModelStatus {
  model_id: string;
  display_name: string;
  approximate_size_bytes: number;
  state: ModelInstallState;
  custom_model_path: string | null;
  language_support: "multilingual" | "english_only" | null;
}

type ModelDownloadEvent =
  | { type: "started"; model_id: string; total_bytes: number }
  | { type: "progress"; model_id: string; downloaded_bytes: number; total_bytes: number; bytes_per_second: number }
  | { type: "verifying"; model_id: string }
  | { type: "completed"; model_id: string; path: string }
  | { type: "cancelled"; model_id: string }
  | { type: "failed"; model_id: string; error: string };

function formatBytes(bytes: number): string {
  if (bytes >= 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
  if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  if (bytes >= 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${bytes} B`;
}

function formatSeconds(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return "calculando...";
  if (seconds < 60) return `${Math.ceil(seconds)}s`;
  const minutes = Math.floor(seconds / 60);
  const remainder = Math.ceil(seconds % 60);
  return `${minutes}min ${remainder}s`;
}

// Onboarding/consentimento → download → verificação → modelo pronto. Nunca baixa
// silenciosamente: o download só começa após o clique explícito em "Baixar e continuar".
// Também cobre a tela de erro (com retry) e, quando pronto, exibe o texto de privacidade.
function ModelOnboardingGate({ children }: { children: ReactNode }) {
  const [status, setStatus] = useState<ModelStatus | null>(null);
  const [statusError, setStatusError] = useState<string | null>(null);
  const [progress, setProgress] = useState<{ downloaded: number; total: number; bytesPerSecond: number } | null>(
    null,
  );

  const refreshStatus = useCallback(() => {
    invoke<ModelStatus>("model_status_command")
      .then((s) => {
        setStatus(s);
        setStatusError(null);
      })
      .catch((e) => setStatusError(String(e)));
  }, []);

  useEffect(() => {
    refreshStatus();
  }, [refreshStatus]);

  useEffect(() => {
    const unlisten = listen<ModelDownloadEvent>("model-download://event", (event) => {
      const payload = event.payload;
      if (payload.type === "progress") {
        setProgress({
          downloaded: payload.downloaded_bytes,
          total: payload.total_bytes,
          bytesPerSecond: payload.bytes_per_second,
        });
      } else if (payload.type === "started") {
        setProgress({ downloaded: 0, total: payload.total_bytes, bytesPerSecond: 0 });
      } else {
        // verifying/completed/cancelled/failed — refresh the authoritative status
        // rather than trying to derive UI state from the event stream alone.
        refreshStatus();
      }
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [refreshStatus]);

  const startDownload = async () => {
    try {
      await invoke("start_model_download_command");
      setStatus((s) => (s ? { ...s, state: { state: "downloading" } } : s));
    } catch (e) {
      setStatusError(String(e));
    }
  };

  const cancelDownload = async () => {
    try {
      await invoke("cancel_model_download_command");
    } catch {
      // Cancellation is best-effort from the UI's perspective — the authoritative
      // outcome arrives via the `cancelled`/`failed` event, which triggers a refresh.
    }
  };

  if (statusError) {
    return (
      <main className="flex min-h-screen flex-col items-center justify-center gap-4 p-6 text-center">
        <p className="text-sm text-red-400">Erro ao verificar o modelo: {statusError}</p>
        <button
          type="button"
          onClick={refreshStatus}
          className="rounded bg-indigo-600 px-4 py-2 text-sm font-medium hover:bg-indigo-500"
        >
          Tentar novamente
        </button>
      </main>
    );
  }

  if (!status) {
    return (
      <main className="flex min-h-screen flex-col items-center justify-center gap-4 p-6 text-center">
        <p className="text-sm text-neutral-400">Verificando modelo de transcrição...</p>
      </main>
    );
  }

  const state = status.state.state;

  if (state === "downloading" || state === "verifying" || state === "installing") {
    const downloaded = progress?.downloaded ?? 0;
    const total = progress?.total ?? status.approximate_size_bytes;
    const percent = total > 0 ? Math.min(100, Math.round((downloaded / total) * 100)) : 0;
    const remainingBytes = Math.max(0, total - downloaded);
    const remainingSeconds =
      progress && progress.bytesPerSecond > 0 ? remainingBytes / progress.bytesPerSecond : NaN;
    const barLength = 15;
    const filled = Math.round((percent / 100) * barLength);
    const bar = "█".repeat(filled) + "░".repeat(barLength - filled);

    return (
      <main className="flex min-h-screen flex-col items-center justify-center gap-4 p-6 text-center">
        <h1 className="text-xl font-semibold">Preparando transcrição local</h1>

        <p className="font-mono text-lg tracking-tight">
          {bar} {percent}%
        </p>

        {state === "downloading" ? (
          <>
            <p className="text-sm text-neutral-400">
              {formatBytes(downloaded)} de {formatBytes(total)}
            </p>
            <p className="text-sm text-neutral-400">
              Velocidade: {progress ? `${formatBytes(progress.bytesPerSecond)}/s` : "calculando..."}
            </p>
            <p className="text-sm text-neutral-400">
              Tempo restante aproximado: {formatSeconds(remainingSeconds)}
            </p>
            <button
              type="button"
              onClick={cancelDownload}
              className="rounded border border-neutral-700 px-4 py-2 text-sm font-medium hover:bg-neutral-800"
            >
              Cancelar
            </button>
          </>
        ) : (
          <p className="text-sm text-neutral-400">
            {state === "verifying" ? "Verificando integridade do arquivo..." : "Instalando..."}
          </p>
        )}
      </main>
    );
  }

  if (state === "failed" || state === "corrupted") {
    const reason = "reason" in status.state ? status.state.reason : "erro desconhecido";
    return (
      <main className="flex min-h-screen flex-col items-center justify-center gap-4 p-6 text-center">
        <h1 className="text-xl font-semibold">Não foi possível baixar o modelo.</h1>
        <p className="max-w-md text-sm text-neutral-400">{reason}</p>
        <div className="flex gap-3">
          <button
            type="button"
            onClick={startDownload}
            className="rounded bg-indigo-600 px-4 py-2 text-sm font-medium hover:bg-indigo-500"
          >
            Tentar novamente
          </button>
          <button
            type="button"
            disabled
            title="Ainda não implementado"
            className="rounded border border-neutral-700 px-4 py-2 text-sm font-medium opacity-50"
          >
            Usar provedor online
          </button>
        </div>
      </main>
    );
  }

  if (state === "ready") {
    return (
      <>
        <p className="mx-auto max-w-md rounded border border-neutral-800 bg-neutral-900/50 p-3 text-xs text-neutral-400">
          O modelo é executado no seu computador.
          <br />
          O áudio não é enviado a terceiros quando a transcrição local está ativa.
        </p>
        {children}
      </>
    );
  }

  // not_installed / checking / cancelled: onboarding + consent screen.
  return (
    <main className="flex min-h-screen flex-col items-center justify-center gap-4 p-6 text-center">
      <h1 className="text-xl font-semibold">Transcrição local e privada</h1>
      <p className="max-w-md text-sm text-neutral-400">
        Para transformar as conversas em texto sem enviar o áudio
        <br />
        para serviços externos, o Helppye precisa baixar um modelo
        <br />
        de transcrição.
      </p>
      <div className="rounded-lg border border-neutral-800 p-4">
        <p className="font-medium">{status.display_name}</p>
        <p className="text-sm text-neutral-400">
          Tamanho aproximado: {formatBytes(status.approximate_size_bytes)}
        </p>
        <p className="text-xs text-neutral-500">Download necessário apenas uma vez.</p>
      </div>
      <button
        type="button"
        onClick={startDownload}
        className="rounded bg-indigo-600 px-4 py-2 text-sm font-medium hover:bg-indigo-500"
      >
        Baixar e continuar
      </button>
    </main>
  );
}

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

function modelNameFromPath(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

// Configurações → Transcrição → Avançado → Usar modelo local personalizado. Fora do
// fluxo principal (por trás de um <details>) — usuários comuns nunca precisam disso.
// `select_custom_model_command` carrega o arquivo de fato antes de persistir a seleção.
function AdvancedTranscriptionSettings() {
  const [modelPath, setModelPath] = useState("");
  const [status, setStatus] = useState<"idle" | "loading" | "loaded" | "error">("idle");
  const [error, setError] = useState<string | null>(null);

  const selectCustomModel = async () => {
    setStatus("loading");
    setError(null);
    try {
      await invoke("select_custom_model_command", {
        modelPath,
        modelName: modelNameFromPath(modelPath),
      });
      setStatus("loaded");
    } catch (e) {
      setStatus("error");
      setError(String(e));
    }
  };

  return (
    <details className="w-full max-w-md rounded-lg border border-neutral-800 p-4 text-left">
      <summary className="cursor-pointer text-sm font-medium text-neutral-300">
        Configurações → Transcrição → Avançado
      </summary>
      <div className="mt-3 flex flex-col gap-2">
        <p className="text-xs text-neutral-500">Usar modelo local personalizado</p>
        <label className="text-xs text-neutral-400">
          Caminho do modelo (.bin, formato ggml/whisper.cpp)
          <input
            type="text"
            value={modelPath}
            onChange={(e) => setModelPath(e.target.value)}
            placeholder="/caminho/para/modelo.bin"
            className="mt-1 w-full rounded border border-neutral-700 bg-neutral-900 px-2 py-1 text-sm text-neutral-200"
          />
        </label>

        <button
          type="button"
          onClick={selectCustomModel}
          disabled={modelPath.length === 0 || status === "loading"}
          className="w-full rounded bg-indigo-600 px-4 py-2 text-sm font-medium hover:bg-indigo-500 disabled:opacity-50"
        >
          {status === "loading" ? "Validando..." : "Usar este modelo"}
        </button>

        {status === "loaded" && <p className="text-xs text-emerald-400">Modelo personalizado validado e configurado.</p>}
        {error && <p className="text-xs text-red-400">{error}</p>}
      </div>
    </details>
  );
}

export default function App() {
  return (
    <ModelOnboardingGate>
      <main className="flex min-h-screen flex-col items-center justify-center gap-4 p-6 text-center">
        <h1 className="text-2xl font-semibold">Helppye</h1>
        <p className="text-sm text-neutral-400">Microphone + system audio capture test</p>

        <AdvancedTranscriptionSettings />

        <div className="flex flex-col gap-4 sm:flex-row">
          {PANELS.map((config) => (
            <CapturePanel key={config.source} config={config} />
          ))}
        </div>
      </main>
    </ModelOnboardingGate>
  );
}
