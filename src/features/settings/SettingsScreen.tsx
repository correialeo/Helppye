import { useState, type ReactNode } from "react";
import { Mic, MonitorSpeaker, Terminal } from "lucide-react";
import { AppShell } from "../../components/layout/AppShell";
import { TextInput } from "../../components/ui/TextInput";
import { Toggle } from "../../components/ui/Toggle";
import { ProviderOption } from "../../components/ui/ProviderOption";
import { SecondaryButton } from "../../components/ui/SecondaryButton";
import { DeviceTestBlock } from "../audio-setup/DeviceTestBlock";
import { OllamaPanel } from "../ai-provider/OllamaPanel";
import { CloudProviderPanel } from "../ai-provider/CloudProviderPanel";
import { useOnboardingStore } from "../../stores/useOnboardingStore";
import { useResponseProvider } from "../../hooks/useResponseProvider";
import type { ResponseProviderKind } from "../../types/responseProvider";

interface SettingsScreenProps {
  onBack: () => void;
  onOpenDeveloperTools: () => void;
}

const PROVIDERS: { value: ResponseProviderKind; name: string; description: string; badge?: string }[] = [
  { value: "ollama", name: "Ollama", description: "Local e privado", badge: "Recomendado" },
  { value: "open_ai", name: "OpenAI", description: "Rápido e preciso" },
  { value: "anthropic", name: "Anthropic", description: "Respostas naturais" },
  { value: "deep_seek", name: "DeepSeek", description: "Alternativa em nuvem" },
];

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="flex flex-col gap-2.5">
      <h2 className="text-xs font-semibold uppercase tracking-wide text-neutral-500">{title}</h2>
      <div className="flex flex-col gap-2">{children}</div>
    </section>
  );
}

/**
 * Every reentrant setting the onboarding covered, plus the one switch that unlocks
 * developer tools — organized into short, labeled sections rather than a single long
 * form. See docs/design-system.md §Complexidade ocultada for what's deliberately absent
 * here (it's all one screen further, behind "Modo de desenvolvedor").
 */
export function SettingsScreen({ onBack, onOpenDeveloperTools }: SettingsScreenProps) {
  const userName = useOnboardingStore((s) => s.userName);
  const setUserName = useOnboardingStore((s) => s.setUserName);
  const devMode = useOnboardingStore((s) => s.devMode);
  const setDevMode = useOnboardingStore((s) => s.setDevMode);
  const [nameDraft, setNameDraft] = useState(userName);

  const { status, saveConfig, saveApiKey, removeApiKey } = useResponseProvider();
  const [selectedProvider, setSelectedProvider] = useState<ResponseProviderKind | null>(null);
  const activeProvider = selectedProvider ?? status?.provider ?? "ollama";

  return (
    <AppShell title="Configurações" onBack={onBack}>
      <div className="flex flex-col gap-6 pb-6">
        <Section title="Perfil">
          <TextInput
            value={nameDraft}
            onChange={(e) => setNameDraft(e.target.value)}
            onBlur={() => setUserName(nameDraft)}
            placeholder="Seu nome"
          />
        </Section>

        <Section title="Idioma">
          <div className="flex items-center justify-between rounded-lg border border-white/10 bg-surface px-3.5 py-2.5 text-sm text-neutral-300">
            Português (Brasil)
          </div>
        </Section>

        <Section title="Áudio">
          <DeviceTestBlock icon={<Mic className="h-4 w-4" />} title="Microfone" source="microphone" />
          <DeviceTestBlock icon={<MonitorSpeaker className="h-4 w-4" />} title="Áudio do computador" source="system_output" />
        </Section>

        <Section title="Sugestão de resposta">
          <div className="flex flex-col gap-2">
            {PROVIDERS.map((provider) => (
              <ProviderOption
                key={provider.value}
                name={provider.name}
                description={provider.description}
                badge={provider.badge}
                selected={activeProvider === provider.value}
                onSelect={() => setSelectedProvider(provider.value)}
              />
            ))}
          </div>
          {status &&
            (activeProvider === "ollama" ? (
              <OllamaPanel status={status} onSave={(config) => saveConfig({ provider: "ollama", ...config })} />
            ) : (
              <CloudProviderPanel
                provider={activeProvider}
                status={status}
                onSaveKey={async (provider, apiKey, model) => {
                  await saveConfig({ provider, model, baseUrl: null, ollamaKeepAlive: null });
                  await saveApiKey(provider, apiKey);
                }}
                onRemoveKey={removeApiKey}
              />
            ))}
        </Section>

        <Section title="Geral">
          <div className="rounded-lg border border-white/10 bg-surface px-3.5 py-2.5">
            <Toggle
              checked={devMode}
              onChange={setDevMode}
              label="Modo de desenvolvedor"
              description="Mostra diagnósticos técnicos detalhados (turnos, latência, eventos)."
            />
          </div>
          {devMode && (
            <SecondaryButton onClick={onOpenDeveloperTools}>
              <Terminal className="h-4 w-4" /> Abrir diagnóstico
            </SecondaryButton>
          )}
        </Section>
      </div>
    </AppShell>
  );
}
