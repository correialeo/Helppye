import { useEffect, useState } from "react";
import { Loader2 } from "lucide-react";
import { StatusIndicator } from "../../components/feedback/StatusIndicator";
import { SecondaryButton } from "../../components/ui/SecondaryButton";
import { Select } from "../../components/ui/Select";
import { TextInput } from "../../components/ui/TextInput";
import { InlineNotice } from "../../components/ui/InlineNotice";
import { useOllamaProbe } from "../../hooks/useOllamaProbe";
import type { ResponseProviderStatus } from "../../types/responseProvider";

const FALLBACK_MODEL = "llama3.1";

interface OllamaPanelProps {
  status: ResponseProviderStatus;
  onSave: (config: { model: string; baseUrl: string | null; ollamaKeepAlive: string | null }) => Promise<void>;
}

/** Ollama's whole pitch is "it just works" — so this panel leads with a real, live
 * connected/not-connected check (see services/ollamaService.ts) instead of assuming
 * success, and only ever shows a URL/keep_alive field behind "Configuração avançada". */
export function OllamaPanel({ status, onSave }: OllamaPanelProps) {
  const isCurrent = status.provider === "ollama";
  const { result, loading, refresh } = useOllamaProbe(status.base_url, true);
  const [editingModel, setEditingModel] = useState(false);
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const [baseUrlDraft, setBaseUrlDraft] = useState(status.base_url ?? "");
  const [keepAliveDraft, setKeepAliveDraft] = useState(status.ollama_keep_alive ?? "");
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    setBaseUrlDraft(status.base_url ?? "");
    setKeepAliveDraft(status.ollama_keep_alive ?? "");
  }, [status.base_url, status.ollama_keep_alive]);

  const chooseModel = async (model: string) => {
    setSaving(true);
    try {
      await onSave({
        model,
        baseUrl: baseUrlDraft.trim() || null,
        ollamaKeepAlive: keepAliveDraft.trim() || null,
      });
      setEditingModel(false);
    } finally {
      setSaving(false);
    }
  };

  const saveAdvanced = async () => {
    setSaving(true);
    try {
      await onSave({
        model: isCurrent ? status.model : FALLBACK_MODEL,
        baseUrl: baseUrlDraft.trim() || null,
        ollamaKeepAlive: keepAliveDraft.trim() || null,
      });
      refresh();
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="flex flex-col gap-3 rounded-lg border border-white/10 bg-surface px-4 py-3.5">
      <div>
        <p className="text-sm font-medium text-neutral-100">Ollama</p>
        <p className="text-xs text-neutral-500">Executa diretamente no seu computador.</p>
      </div>

      <div className="flex items-center justify-between">
        {loading ? (
          <StatusIndicator label="Verificando..." tone="neutral" />
        ) : result?.reachable ? (
          <StatusIndicator label="Conectado" tone="active" pulse={false} />
        ) : (
          <StatusIndicator label="Ollama não encontrado" tone="warning" />
        )}
        {loading && <Loader2 className="h-3.5 w-3.5 animate-spin text-neutral-500" />}
      </div>

      {isCurrent && !editingModel && (
        <div className="flex items-center justify-between">
          <p className="text-xs text-neutral-400">
            Modelo: <span className="text-neutral-200">{status.model}</span>
          </p>
          <button
            type="button"
            className="text-xs font-medium text-brand-400 hover:text-brand-300"
            onClick={() => setEditingModel(true)}
          >
            Alterar modelo
          </button>
        </div>
      )}

      {(editingModel || !isCurrent) &&
        (result?.reachable && result.models.length > 0 ? (
          <Select
            value={isCurrent ? status.model : null}
            onChange={chooseModel}
            options={result.models.map((name) => ({ value: name, label: name }))}
            placeholder="Escolher modelo instalado"
            disabled={saving}
          />
        ) : (
          <div className="flex gap-2">
            <TextInput
              placeholder="ex.: llama3.1, qwen3:8b..."
              defaultValue={isCurrent ? status.model : ""}
              onKeyDown={(e) => {
                if (e.key === "Enter") chooseModel((e.target as HTMLInputElement).value);
              }}
              className="flex-1"
            />
          </div>
        ))}

      {!result?.reachable && !loading && (
        <InlineNotice tone="warning">
          Não encontramos o Ollama em {status.base_url ?? "http://localhost:11434"}. Instale e abra o Ollama, depois
          tente de novo.
          <button type="button" className="ml-1 underline underline-offset-2" onClick={refresh}>
            Tentar novamente
          </button>
        </InlineNotice>
      )}

      <details
        className="text-xs text-neutral-500"
        open={advancedOpen}
        onToggle={(e) => setAdvancedOpen((e.target as HTMLDetailsElement).open)}
      >
        <summary className="cursor-pointer select-none text-neutral-500 hover:text-neutral-300">
          Configuração avançada
        </summary>
        <div className="mt-2 flex flex-col gap-2">
          <TextInput
            label="URL do servidor"
            value={baseUrlDraft}
            onChange={(e) => setBaseUrlDraft(e.target.value)}
            placeholder="http://localhost:11434"
          />
          <TextInput
            label="Manter modelo carregado (keep_alive)"
            value={keepAliveDraft}
            onChange={(e) => setKeepAliveDraft(e.target.value)}
            placeholder="10m"
          />
          <SecondaryButton onClick={saveAdvanced} disabled={saving}>
            Salvar
          </SecondaryButton>
        </div>
      </details>
    </div>
  );
}
