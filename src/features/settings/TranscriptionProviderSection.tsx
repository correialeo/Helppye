import { useEffect, useState } from "react";
import { StatusIndicator, type StatusTone } from "../../components/feedback/StatusIndicator";
import { InlineNotice } from "../../components/ui/InlineNotice";
import { PasswordInput } from "../../components/ui/PasswordInput";
import { PrimaryButton } from "../../components/ui/PrimaryButton";
import { ProviderOption } from "../../components/ui/ProviderOption";
import { SecondaryButton } from "../../components/ui/SecondaryButton";
import { TextInput } from "../../components/ui/TextInput";
import { useTranscriptionProvider } from "../../hooks/useTranscriptionProvider";
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
