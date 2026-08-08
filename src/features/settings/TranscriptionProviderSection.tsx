import { useEffect, useState } from "react";
import { StatusIndicator, type StatusTone } from "../../components/feedback/StatusIndicator";
import { InlineNotice } from "../../components/ui/InlineNotice";
import { PasswordInput } from "../../components/ui/PasswordInput";
import { PrimaryButton } from "../../components/ui/PrimaryButton";
import { ProviderOption } from "../../components/ui/ProviderOption";
import { SecondaryButton } from "../../components/ui/SecondaryButton";
import { TextInput } from "../../components/ui/TextInput";
import { useTranscriptionProvider } from "../../hooks/useTranscriptionProvider";
import { useModelStatus } from "../../hooks/useModelStatus";
import { formatBytes, formatSeconds } from "../../utils/format";
import type {
  TranscriptionConnectionState,
  TranscriptionProviderDescriptor,
} from "../../types/transcriptionProvider";

const STATUS_COPY: Record<
  TranscriptionConnectionState,
  { label: string; tone: StatusTone; pulse?: boolean }
> = {
  not_configured: { label: "Não configurado", tone: "neutral" },
  connecting: { label: "Conectando", tone: "neutral", pulse: true },
  connected: { label: "Conectado", tone: "active" },
  error: { label: "Erro", tone: "error" },
};

function descriptionFor(descriptor: TranscriptionProviderDescriptor): string {
  if (descriptor.id === "whisper_local") return "Privado e executado neste dispositivo.";
  if (descriptor.id === "google_gemini") return "Streaming remoto com transcrições parciais.";
  return descriptor.unavailable_reason ?? "Provider remoto de transcrição.";
}

export function TranscriptionProviderSection() {
  const {
    descriptors,
    settings,
    hasGeminiKey,
    connectionState,
    error,
    activateLocal,
    connectGemini,
    removeGeminiKey,
  } = useTranscriptionProvider();
  const localModel = useModelStatus();
  const [selected, setSelected] = useState<"whisper_local" | "google_gemini">("whisper_local");
  const [apiKey, setApiKey] = useState("");
  const [model, setModel] = useState("");

  useEffect(() => {
    if (!settings) return;
    if (settings.provider === "google_gemini") setSelected("google_gemini");
    setModel(settings.providers.google_gemini.model);
  }, [settings]);

  const local = descriptors.filter((descriptor) => descriptor.capabilities.local);
  const cloud = descriptors.filter((descriptor) => !descriptor.capabilities.local);
  const status = STATUS_COPY[connectionState];
  const busy = connectionState === "connecting";
  const installingModel = localModel.models.find((model) =>
    model.state.state === "downloading" || model.state.state === "verifying" || model.state.state === "installing",
  );
  const modelState = installingModel?.state.state ?? localModel.status?.state.state;
  const modelBusy = Boolean(installingModel);
  const downloaded = localModel.progress?.downloaded ?? 0;
  const total = localModel.progress?.total ?? installingModel?.approximate_size_bytes ?? 0;
  const downloadPercent = total > 0 ? Math.min(100, Math.round((downloaded / total) * 100)) : 0;

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-col gap-2">
        <p className="text-[11px] font-semibold uppercase tracking-wide text-neutral-500">Local</p>
        {local.map((descriptor) => (
          <ProviderOption
            key={descriptor.id}
            name={descriptor.display_name}
            description={descriptionFor(descriptor)}
            selected={selected === "whisper_local" && descriptor.id === "whisper_local"}
            disabled={!descriptor.available}
            badge={descriptor.available ? undefined : "Em breve"}
            onSelect={() => {
              void activateLocal().then((activated) => {
                if (activated) setSelected("whisper_local");
              });
            }}
          />
        ))}
      </div>

      <div className="flex flex-col gap-3 rounded-lg border border-white/10 bg-surface px-4 py-3.5">
        <div className="flex items-start justify-between gap-3">
          <div>
            <p className="text-sm font-medium text-neutral-100">Modelo local</p>
            <p className="mt-1 text-xs text-neutral-500">
              {localModel.status?.display_name
                ? `Último selecionado: ${localModel.status.display_name}`
                : "Verificando modelos instalados..."}
            </p>
          </div>
          <span className="rounded-full border border-white/10 px-2 py-1 text-[10px] font-semibold uppercase tracking-wide text-neutral-500">
            Offline
          </span>
        </div>
        <div className="flex flex-col gap-2">
          {localModel.models.length === 0 && (
            <p className="rounded-md border border-white/8 bg-black/15 px-3 py-2.5 text-xs text-neutral-500">
              Verificando modelos instalados...
            </p>
          )}
          {localModel.models.map((model) => {
            const state = model.state.state;
            const selected = localModel.status?.model_id === model.model_id;
            const installed = state === "ready";
            const downloading = state === "downloading";
            const description = model.model_id === "whisper-large-v3-turbo"
              ? "Mais rápido para comparar com o Gemini Live."
              : "Equilíbrio entre tamanho e qualidade para uso local.";

            return (
              <div key={model.model_id} className="rounded-md border border-white/8 bg-black/15 px-3 py-2.5">
                <div className="flex items-center justify-between gap-3">
                  <div className="min-w-0">
                    <p className="text-sm font-medium text-neutral-200">{model.display_name}</p>
                    <p className="text-xs text-neutral-500">
                      {description} · {formatBytes(model.approximate_size_bytes)}
                    </p>
                    <p className="mt-1 text-[11px] font-medium text-neutral-500">
                      {selected && installed ? "Selecionado" : installed ? "Instalado" : state === "corrupted" ? "Arquivo inválido" : state === "failed" ? "Falha no download" : "Não instalado"}
                    </p>
                  </div>
                  <SecondaryButton
                    className="shrink-0 px-3 py-2 text-xs"
                    onClick={() => {
                      if (downloading) {
                        void localModel.cancelDownload();
                      } else if (installed) {
                        void localModel.selectModel(model.model_id);
                      } else {
                        void localModel.startDownload(model.model_id);
                      }
                    }}
                    disabled={(modelBusy && !downloading) || (selected && installed)}
                  >
                    {selected && installed
                      ? "Selecionado"
                      : downloading
                        ? "Cancelar"
                        : state === "verifying" || state === "installing"
                          ? "Instalando..."
                          : installed
                            ? "Selecionar"
                            : state === "failed" || state === "corrupted" || state === "cancelled"
                              ? "Tentar novamente"
                              : "Baixar"}
                  </SecondaryButton>
                </div>
              </div>
            );
          })}
        </div>
        {modelBusy && (
          <div className="flex flex-col gap-1.5">
            <div className="h-1.5 overflow-hidden rounded-full bg-white/8" role="progressbar" aria-valuenow={downloadPercent} aria-valuemin={0} aria-valuemax={100}>
              <div className="h-full rounded-full bg-brand-500 transition-[width] duration-200" style={{ width: `${downloadPercent}%` }} />
            </div>
            <p className="text-[11px] text-neutral-500">
              {modelState === "downloading"
                ? `${formatBytes(downloaded)} de ${formatBytes(total)} · ${downloadPercent}%${localModel.progress && localModel.progress.bytesPerSecond > 0 ? ` · ${formatSeconds(Math.max(0, total - downloaded) / localModel.progress.bytesPerSecond)}` : ""}`
                : modelState === "verifying" ? "Verificando integridade..." : "Instalando..."}
            </p>
          </div>
        )}
        {localModel.error && <InlineNotice tone="error">{localModel.error}</InlineNotice>}
        <p className="text-xs text-neutral-500">O último modelo selecionado fica salvo e será restaurado ao abrir o app novamente.</p>
      </div>

      <div className="flex flex-col gap-2">
        <p className="text-[11px] font-semibold uppercase tracking-wide text-neutral-500">Cloud</p>
        {cloud.map((descriptor) => (
          <ProviderOption
            key={descriptor.id}
            name={descriptor.display_name}
            description={descriptionFor(descriptor)}
            selected={
              descriptor.id === "google_gemini"
                ? selected === "google_gemini"
                : settings?.provider === descriptor.id
            }
            disabled={!descriptor.available}
            badge={descriptor.available ? undefined : "Em breve"}
            onSelect={() => {
              if (descriptor.id === "google_gemini") setSelected("google_gemini");
            }}
          />
        ))}
      </div>

      {selected === "google_gemini" && settings && (
        <div className="flex flex-col gap-3 rounded-lg border border-white/10 bg-surface px-4 py-3.5">
          <div className="flex items-center justify-between gap-3">
            <p className="text-sm font-medium text-neutral-100">Gemini Live</p>
            <StatusIndicator label={status.label} tone={status.tone} pulse={status.pulse} />
          </div>
          <p className="text-xs text-neutral-500">
            A chave é salva somente no keychain do sistema. O áudio é enviado ao Google.
          </p>
          <PasswordInput
            placeholder={hasGeminiKey ? "Chave já salva — digite para substituir" : "Gemini API Key"}
            value={apiKey}
            onChange={(event) => setApiKey(event.target.value)}
          />
          <TextInput
            label="Modelo"
            value={model}
            onChange={(event) => setModel(event.target.value)}
          />
          <TextInput
            label="Endpoint oficial"
            value={settings.providers.google_gemini.endpoint}
            readOnly
            disabled
          />
          <p className="text-xs text-neutral-500">Idioma detectado automaticamente pelo Gemini Live.</p>
          {error && <InlineNotice tone="error">{error}</InlineNotice>}
          <div className="flex gap-2">
            <PrimaryButton
              onClick={async () => {
                await connectGemini(apiKey, model);
                setApiKey("");
              }}
              disabled={busy || model.trim().length === 0 || (!hasGeminiKey && !apiKey.trim())}
              loading={busy}
            >
              Testar conexão
            </PrimaryButton>
            {hasGeminiKey && (
              <SecondaryButton onClick={() => void removeGeminiKey()} disabled={busy}>
                Remover chave
              </SecondaryButton>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
