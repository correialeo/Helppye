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

type ConversationSpeaker = "user" | "other_person";

interface ConversationTurn {
  id: number;
  source: AudioSourceKind;
  speaker: ConversationSpeaker;
  text: string;
  utterances: number[];
  started_at: number;
  ended_at: number;
  finalized_at: number | null;
}

interface ConversationUtterance {
  id: number;
  source: AudioSourceKind;
  speaker: ConversationSpeaker;
  text: string;
  segments: number[];
  started_at: number;
  ended_at: number;
  finalized_at: number | null;
}

interface ConversationTimelineSnapshot {
  turns: ConversationTurn[];
  utterances: ConversationUtterance[];
}

type ConversationTimelineEvent =
  | { type: "turn_started"; turn_id: number; speaker: ConversationSpeaker; source: AudioSourceKind; started_at: number }
  | { type: "turn_updated"; turn: ConversationTurn }
  | { type: "turn_finalized"; turn: ConversationTurn }
  | {
      type: "utterance_started";
      utterance_id: number;
      turn_id: number;
      speaker: ConversationSpeaker;
      source: AudioSourceKind;
      started_at: number;
    }
  | {
      type: "utterance_updated";
      utterance_id: number;
      turn_id: number;
      speaker: ConversationSpeaker;
      source: AudioSourceKind;
      started_at: number;
      text: string;
      ended_at: number;
      segments: number[];
    }
  | { type: "utterance_finalized"; turn_id: number; utterance: ConversationUtterance };

function rmsDbfs(samples: number[]): number {
  if (samples.length === 0) return -Infinity;
  const meanSquare = samples.reduce((sum, s) => sum + s * s, 0) / samples.length;
  const rms = Math.sqrt(meanSquare);
  return rms > 0 ? 20 * Math.log10(rms) : -Infinity;
}

type ResolutionSource = "persisted" | "windows_default" | "first_available_fallback";

interface ResolvedDevice {
  device_id: string;
  device_name: string;
  is_windows_default: boolean;
  source: ResolutionSource;
}

interface DeviceSelectionSnapshot {
  input: ResolvedDevice | null;
  output: ResolvedDevice | null;
}

interface PanelConfig {
  source: AudioSourceKind;
  title: string;
  listDevicesCommand: string;
  selectDeviceCommand: string;
  startCommand: string;
  stopCommand: string;
}

const PANELS: readonly [PanelConfig, PanelConfig] = [
  {
    source: "microphone",
    title: "Entrada",
    listDevicesCommand: "list_audio_devices_command",
    selectDeviceCommand: "select_input_device_command",
    startCommand: "start_microphone_capture_command",
    stopCommand: "stop_microphone_capture_command",
  },
  {
    source: "system_output",
    title: "Saída",
    listDevicesCommand: "list_system_audio_devices_command",
    selectDeviceCommand: "select_output_device_command",
    startCommand: "start_system_audio_capture_command",
    stopCommand: "stop_system_audio_capture_command",
  },
];

type PanelStatus =
  | { kind: "parado" }
  | { kind: "capturando" }
  | { kind: "trocando" }
  | { kind: "desconectado" }
  | { kind: "erro"; message: string };

function statusLabel(status: PanelStatus): string {
  switch (status.kind) {
    case "capturando":
      return "Capturando";
    case "parado":
      return "Parado";
    case "desconectado":
      return "Dispositivo desconectado";
    case "trocando":
      return "Trocando dispositivo...";
    case "erro":
      return "Erro";
  }
}

function selectionLabel(source: ResolutionSource | null): string {
  if (source === "windows_default") return "Padrão do Windows";
  if (source === "first_available_fallback") {
    return "Nenhum dispositivo padrão encontrado — usando o primeiro disponível";
  }
  return "Seleção manual";
}

// Exatamente 1 dispositivo selecionado por vez para esta categoria (entrada OU saída) —
// um único <select>, nunca multi-select. Parar → esperar o encerramento real → reabrir
// no novo dispositivo é responsabilidade do backend (`CaptureEngine::select_device`);
// este painel só precisa exibir "Trocando dispositivo..." enquanto aguarda essa promessa
// e suprimir os eventos started/stopped que chegam como parte dessa troca interna, para
// não piscar entre estados intermediários.
function DeviceCapturePanel({
  config,
  initialSelection,
}: {
  config: PanelConfig;
  initialSelection: ResolvedDevice | null;
}) {
  const [devices, setDevices] = useState<AudioDevice[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(initialSelection?.device_id ?? null);
  const [selectionSource, setSelectionSource] = useState<ResolutionSource | null>(initialSelection?.source ?? null);
  const [status, setStatus] = useState<PanelStatus>({ kind: "parado" });
  const [levelDb, setLevelDb] = useState(-Infinity);
  const [frameCount, setFrameCount] = useState(0);
  const [suggestedDevice, setSuggestedDevice] = useState<ResolvedDevice | null>(null);
  const levelDecayTimer = useRef<number | null>(null);
  const switchingRef = useRef(false);

  const refreshDevices = useCallback(() => {
    invoke<AudioDevice[]>(config.listDevicesCommand)
      .then(setDevices)
      .catch((e) => setStatus({ kind: "erro", message: String(e) }));
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
      if (switchingRef.current) return;

      if (payload.type === "started") {
        setStatus({ kind: "capturando" });
      } else if (payload.type === "frame") {
        // Throttled by design: the level meter only updates on frame arrival (100ms
        // frames from CaptureConfig::default), and decays to silence if frames stop.
        setLevelDb(rmsDbfs(payload.samples));
        setFrameCount((n) => n + 1);
        if (levelDecayTimer.current !== null) window.clearTimeout(levelDecayTimer.current);
        levelDecayTimer.current = window.setTimeout(() => setLevelDb(-Infinity), 300);
      } else if (payload.type === "error") {
        setStatus({ kind: "erro", message: payload.message });
      } else if (payload.type === "device_disconnected") {
        setStatus({ kind: "desconectado" });
        setLevelDb(-Infinity);
        refreshDevices();
        // The backend already stopped capture and re-persisted a fallback selection on
        // its own (`CaptureEngine::resolve_and_persist`) — never auto-restart, just
        // reflect that fallback here as an opt-in suggestion for the user.
        invoke<DeviceSelectionSnapshot>("resolve_device_selection_command")
          .then((snapshot) => {
            const resolved = config.source === "microphone" ? snapshot.input : snapshot.output;
            setSuggestedDevice(resolved);
            setSelectedId(resolved?.device_id ?? null);
            setSelectionSource(resolved?.source ?? null);
          })
          .catch(() => {});
      } else if (payload.type === "stopped") {
        setStatus((s) => (s.kind === "desconectado" || s.kind === "erro" ? s : { kind: "parado" }));
      }
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [config.source, refreshDevices]);

  const handleSelectDevice = async (deviceId: string) => {
    const wasCapturing = status.kind === "capturando";
    setSelectedId(deviceId);
    setSelectionSource(null);
    setSuggestedDevice(null);
    if (wasCapturing) {
      switchingRef.current = true;
      setStatus({ kind: "trocando" });
    }
    try {
      await invoke(config.selectDeviceCommand, { deviceId });
      setStatus(wasCapturing ? { kind: "capturando" } : { kind: "parado" });
    } catch (e) {
      setStatus({ kind: "erro", message: String(e) });
    } finally {
      switchingRef.current = false;
    }
  };

  const startCapture = async () => {
    setFrameCount(0);
    setSuggestedDevice(null);
    try {
      await invoke(config.startCommand);
      setStatus({ kind: "capturando" });
    } catch (e) {
      setStatus({ kind: "erro", message: String(e) });
    }
  };

  const stopCapture = async () => {
    try {
      await invoke(config.stopCommand);
      setStatus({ kind: "parado" });
    } catch (e) {
      setStatus({ kind: "erro", message: String(e) });
    }
  };

  const useSuggestedDevice = async () => {
    if (!suggestedDevice) return;
    setSuggestedDevice(null);
    await startCapture();
  };

  const levelPercent = Number.isFinite(levelDb) ? Math.min(100, Math.max(0, (levelDb + 60) * (100 / 60))) : 0;
  const noDevices = devices.length === 0;
  const controlsDisabled = noDevices || status.kind === "trocando";

  return (
    <section className="flex w-full max-w-xs flex-col items-center gap-3 rounded-lg border border-neutral-800 p-4">
      <div className="text-center">
        <h2 className="text-base font-semibold">{config.title}</h2>
        <p className="text-xs text-neutral-500">{statusLabel(status)}</p>
      </div>

      <label className="w-full text-left text-xs text-neutral-400">
        Dispositivo
        <select
          value={selectedId ?? ""}
          onChange={(e) => handleSelectDevice(e.target.value)}
          disabled={controlsDisabled}
          className="mt-1 w-full rounded border border-neutral-700 bg-neutral-900 px-2 py-1 text-sm text-neutral-200 disabled:opacity-50"
        >
          {noDevices && <option value="">Nenhum dispositivo encontrado</option>}
          {devices.map((d) => (
            <option key={d.id} value={d.id}>
              {d.name}
              {d.is_default ? " (padrão do Windows)" : ""}
            </option>
          ))}
        </select>
      </label>
      <p className="w-full text-left text-xs text-neutral-500">{selectionLabel(selectionSource)}</p>

      <button
        type="button"
        onClick={status.kind === "capturando" ? stopCapture : startCapture}
        disabled={controlsDisabled}
        className="w-full rounded bg-indigo-600 px-4 py-2 text-sm font-medium hover:bg-indigo-500 disabled:opacity-50"
      >
        {status.kind === "capturando" ? "Parar captura" : "Iniciar captura"}
      </button>

      <div className="w-full">
        <div className="h-3 w-full overflow-hidden rounded bg-neutral-800">
          <div className="h-full bg-emerald-500 transition-all" style={{ width: `${levelPercent}%` }} />
        </div>
        <p className="mt-1 text-xs text-neutral-500">
          {status.kind === "capturando" ? `${frameCount} frames` : "idle"} —{" "}
          {Number.isFinite(levelDb) ? `${levelDb.toFixed(1)} dBFS` : "-∞ dBFS"}
        </p>
      </div>

      {status.kind === "erro" && <p className="text-xs text-red-400">{status.message}</p>}

      {status.kind === "desconectado" && (
        <div className="w-full rounded border border-amber-700 bg-amber-950/30 p-2 text-left text-xs text-amber-300">
          <p>
            Dispositivo desconectado.
            {suggestedDevice ? ` Dispositivo sugerido: ${suggestedDevice.device_name}.` : ""}
          </p>
          {suggestedDevice && (
            <button
              type="button"
              onClick={useSuggestedDevice}
              className="mt-2 rounded border border-amber-700 px-2 py-1 text-xs font-medium hover:bg-amber-900/40"
            >
              [Usar este dispositivo]
            </button>
          )}
        </div>
      )}

    </section>
  );
}

function formatTimelineTime(ms: number): string {
  const totalSeconds = Math.floor(ms / 1000);
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  return `${minutes}:${seconds.toString().padStart(2, "0")}`;
}

function speakerLabel(speaker: ConversationSpeaker): string {
  return speaker === "user" ? "Você" : "Outra pessoa";
}

function sourceLabel(source: AudioSourceKind): string {
  return source === "microphone" ? "Entrada" : "Saída";
}

function sortTurns(turns: ConversationTurn[]): ConversationTurn[] {
  return [...turns].sort(
    (a, b) =>
      a.started_at - b.started_at ||
      a.ended_at - b.ended_at ||
      a.id - b.id,
  );
}

function sortUtterances(utterances: ConversationUtterance[]): ConversationUtterance[] {
  return [...utterances].sort(
    (a, b) =>
      a.started_at - b.started_at ||
      a.ended_at - b.ended_at ||
      a.id - b.id,
  );
}

function ConversationTimelineView() {
  const [turns, setTurns] = useState<ConversationTurn[]>([]);
  const [utterances, setUtterances] = useState<ConversationUtterance[]>([]);
  const [error, setError] = useState<string | null>(null);
  const showSegments = import.meta.env.DEV;

  useEffect(() => {
    invoke<ConversationTimelineSnapshot>("conversation_timeline_snapshot_command")
      .then((snapshot) => {
        setTurns(sortTurns(snapshot.turns));
        setUtterances(sortUtterances(snapshot.utterances));
        setError(null);
      })
      .catch((e) => setError(String(e)));
  }, []);

  useEffect(() => {
    const unlisten = listen<ConversationTimelineEvent>("conversation://timeline-event", (event) => {
      const payload = event.payload;
      if (payload.type === "turn_started") {
        setTurns((current) => {
          const nextTurn: ConversationTurn = {
            id: payload.turn_id,
            speaker: payload.speaker,
            source: payload.source,
            text: "",
            utterances: [],
            started_at: payload.started_at,
            ended_at: payload.started_at,
            finalized_at: null,
          };
          const withoutDuplicate = current.filter((turn) => turn.id !== nextTurn.id);
          return sortTurns([...withoutDuplicate, nextTurn]);
        });
        return;
      }

      if (payload.type === "turn_updated" || payload.type === "turn_finalized") {
        setTurns((current) => {
          const withoutDuplicate = current.filter((turn) => turn.id !== payload.turn.id);
          return sortTurns([...withoutDuplicate, payload.turn]);
        });
        return;
      }

      if (payload.type === "utterance_started") {
        setUtterances((current) => {
          const nextUtterance: ConversationUtterance = {
            id: payload.utterance_id,
            speaker: payload.speaker,
            source: payload.source,
            text: "",
            segments: [],
            started_at: payload.started_at,
            ended_at: payload.started_at,
            finalized_at: null,
          };
          const withoutDuplicate = current.filter((utterance) => utterance.id !== nextUtterance.id);
          return sortUtterances([...withoutDuplicate, nextUtterance]);
        });
        return;
      }

      if (payload.type === "utterance_updated") {
        setUtterances((current) => {
          const existing = current.find((utterance) => utterance.id === payload.utterance_id);
          const nextUtterance: ConversationUtterance = {
            id: payload.utterance_id,
            speaker: existing?.speaker ?? payload.speaker,
            source: existing?.source ?? payload.source,
            text: payload.text,
            segments: payload.segments,
            started_at: existing?.started_at ?? payload.started_at,
            ended_at: payload.ended_at,
            finalized_at: existing?.finalized_at ?? null,
          };
          const withoutDuplicate = current.filter((utterance) => utterance.id !== nextUtterance.id);
          return sortUtterances([...withoutDuplicate, nextUtterance]);
        });
        return;
      }

      setUtterances((current) => {
        const withoutDuplicate = current.filter((utterance) => utterance.id !== payload.utterance.id);
        return sortUtterances([...withoutDuplicate, payload.utterance]);
      });
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  return (
    <section className="flex w-full max-w-3xl flex-col gap-3 rounded-lg border border-neutral-800 p-4 text-left">
      <div>
        <h2 className="text-base font-semibold">Timeline da conversa</h2>
        <p className="text-xs text-neutral-500">
          {utterances.length} blocos · {turns.length} turnos
        </p>
      </div>

      {error && <p className="text-xs text-red-400">Erro ao carregar timeline: {error}</p>}

      <div className="max-h-72 min-h-24 overflow-y-auto border-t border-neutral-800 pt-3">
        {utterances.length === 0 ? (
          <p className="text-xs text-neutral-500">Aguardando transcrições...</p>
        ) : (
          <ol className="flex flex-col gap-3">
            {utterances.map((utterance) => (
              <li key={utterance.id} className="grid grid-cols-[4.5rem_1fr] gap-3 text-sm">
                <div className="text-xs text-neutral-500">
                  <p>{formatTimelineTime(utterance.started_at)}</p>
                  <p>{sourceLabel(utterance.source)}</p>
                  {!utterance.finalized_at && <p className="text-emerald-400">aberto</p>}
                </div>
                <div>
                  <p className="text-xs font-medium text-neutral-300">{speakerLabel(utterance.speaker)}</p>
                  <p className="text-neutral-100">
                    {utterance.text || <span className="text-neutral-500">...</span>}
                  </p>
                  {showSegments && utterance.segments.length > 0 && (
                    <details className="mt-1 text-xs text-neutral-500">
                      <summary className="cursor-pointer">Segmentos internos</summary>
                      <p className="mt-1 font-mono">{utterance.segments.join(", ")}</p>
                    </details>
                  )}
                </div>
              </li>
            ))}
          </ol>
        )}
      </div>

      {showSegments && turns.length > 0 && (
        <details className="border-t border-neutral-800 pt-3 text-xs text-neutral-500">
          <summary className="cursor-pointer">Turnos consolidados</summary>
          <div className="mt-3 flex flex-col gap-3">
            {turns.map((turn) => (
              <div key={turn.id} className="rounded border border-neutral-800 p-2">
                <p className="font-medium text-neutral-300">
                  Turno {turn.id} · {speakerLabel(turn.speaker)} · {turn.utterances.length} blocos
                </p>
                <p className="mt-1 text-neutral-400">{turn.text}</p>
                <div className="mt-2 flex flex-col gap-1">
                  {turn.utterances.map((utteranceId) => {
                    const utterance = utterances.find((u) => u.id === utteranceId);
                    return (
                      <p key={utteranceId} className="font-mono">
                        └─ Utterance {utteranceId}
                        {utterance ? ` · segments: ${utterance.segments.join(", ")}` : ""}
                      </p>
                    );
                  })}
                </div>
              </div>
            ))}
          </div>
        </details>
      )}
    </section>
  );
}

// Resolve (sem iniciar captura) a seleção de entrada e de saída uma única vez, ao montar
// — pré-seleciona o padrão do Windows (ou o fallback) e espera a ação explícita do
// usuário para efetivamente abrir qualquer dispositivo.
function AudioCaptureSection() {
  const [selection, setSelection] = useState<DeviceSelectionSnapshot | null>(null);
  const [resolveError, setResolveError] = useState<string | null>(null);

  useEffect(() => {
    invoke<DeviceSelectionSnapshot>("resolve_device_selection_command")
      .then(setSelection)
      .catch((e) => setResolveError(String(e)));
  }, []);

  if (resolveError) {
    return <p className="text-sm text-red-400">Erro ao resolver dispositivos de áudio: {resolveError}</p>;
  }

  if (!selection) {
    return <p className="text-sm text-neutral-400">Resolvendo dispositivos de áudio...</p>;
  }

  const [inputPanel, outputPanel] = PANELS;

  return (
    <div className="flex flex-col gap-4 sm:flex-row">
      <DeviceCapturePanel config={inputPanel} initialSelection={selection.input} />
      <DeviceCapturePanel config={outputPanel} initialSelection={selection.output} />
    </div>
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

        <AudioCaptureSection />
        <ConversationTimelineView />
      </main>
    </ModelOnboardingGate>
  );
}
