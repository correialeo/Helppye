import { useState } from "react";
import { PasswordInput } from "../../components/ui/PasswordInput";
import { TextInput } from "../../components/ui/TextInput";
import { PrimaryButton } from "../../components/ui/PrimaryButton";
import { SecondaryButton } from "../../components/ui/SecondaryButton";
import { InlineNotice } from "../../components/ui/InlineNotice";
import type { ResponseProviderKind, ResponseProviderStatus } from "../../types/responseProvider";

/** Provedores de nuvem que esta tela oferece. LM Studio e endpoint personalizado existem
 * no backend mas não aparecem aqui: os dois são configuração de endpoint, não "conecte sua
 * conta com uma API key", e o onboarding não é o lugar de pedir uma URL. */
export type CloudProviderKind = "open_ai" | "anthropic" | "deep_seek" | "open_router";

const PROVIDER_COPY: Record<CloudProviderKind, { name: string; modelPlaceholder: string }> = {
  open_ai: { name: "OpenAI", modelPlaceholder: "gpt-4o-mini" },
  anthropic: { name: "Anthropic", modelPlaceholder: "claude-sonnet-5" },
  deep_seek: { name: "DeepSeek", modelPlaceholder: "deepseek-chat" },
  open_router: { name: "OpenRouter", modelPlaceholder: "openai/gpt-4o-mini" },
};

/** Estreita o provedor vindo do backend para os que este painel sabe apresentar. Sem isto
 * a tela mostraria um campo "sk-..." para LM Studio, que nem sempre pede credencial. */
export function isCloudProvider(provider: ResponseProviderKind): provider is CloudProviderKind {
  return provider in PROVIDER_COPY;
}

interface CloudProviderPanelProps {
  provider: CloudProviderKind;
  status: ResponseProviderStatus;
  onSaveKey: (provider: ResponseProviderKind, apiKey: string, model: string) => Promise<void>;
  onRemoveKey: (provider: ResponseProviderKind) => Promise<void>;
}

/** "Conecte sua conta usando uma API key" — never a URL/token/streaming field in the
 * primary view. The key is written straight to the OS keychain (never persisted here)
 * as soon as "Conectar" is pressed; this component never keeps the key in state a
 * moment longer than the input needs it. */
export function CloudProviderPanel({ provider, status, onSaveKey, onRemoveKey }: CloudProviderPanelProps) {
  const copy = PROVIDER_COPY[provider];
  const isCurrent = status.provider === provider;
  const alreadyConnected = isCurrent && status.has_api_key;

  const [apiKey, setApiKey] = useState("");
  const [model, setModel] = useState(isCurrent ? status.model : "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const connect = async () => {
    setError(null);
    setBusy(true);
    try {
      await onSaveKey(provider, apiKey, model.trim() || copy.modelPlaceholder);
      setApiKey("");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    setBusy(true);
    try {
      await onRemoveKey(provider);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="flex flex-col gap-3 rounded-lg border border-white/10 bg-surface px-4 py-3.5">
      <div>
        <p className="text-sm font-medium text-neutral-100">{copy.name}</p>
        <p className="text-xs text-neutral-500">Conecte sua conta usando uma API key.</p>
      </div>

      {alreadyConnected ? (
        <>
          <p className="text-xs text-emerald-400">Chave configurada · Modelo: {status.model}</p>
          <SecondaryButton onClick={remove} disabled={busy}>
            Remover
          </SecondaryButton>
        </>
      ) : (
        <>
          <PasswordInput placeholder="sk-..." value={apiKey} onChange={(e) => setApiKey(e.target.value)} />
          <TextInput
            placeholder={copy.modelPlaceholder}
            value={model}
            onChange={(e) => setModel(e.target.value)}
            hint="Deixe em branco para usar o modelo recomendado."
          />
          {error && <InlineNotice tone="error">{error}</InlineNotice>}
          <PrimaryButton onClick={connect} disabled={apiKey.trim().length === 0 || busy} loading={busy}>
            Conectar
          </PrimaryButton>
        </>
      )}
    </div>
  );
}
