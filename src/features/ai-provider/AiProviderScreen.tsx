import { useState } from "react";
import { OnboardingLayout } from "../../components/layout/OnboardingLayout";
import { ProviderOption } from "../../components/ui/ProviderOption";
import { ONBOARDING_STEPS, onboardingStepIndex } from "../../app/appFlow";
import { useResponseProvider } from "../../hooks/useResponseProvider";
import { OllamaPanel } from "./OllamaPanel";
import { CloudProviderPanel, isCloudProvider } from "./CloudProviderPanel";
import type { ResponseProviderKind } from "../../types/responseProvider";

interface AiProviderScreenProps {
  onBack: () => void;
  onContinue: () => void;
}

const PROVIDERS: { value: ResponseProviderKind; name: string; description: string; badge?: string }[] = [
  { value: "ollama", name: "Ollama", description: "Local e privado", badge: "Recomendado" },
  { value: "open_ai", name: "OpenAI", description: "Rápido e preciso" },
  { value: "anthropic", name: "Anthropic", description: "Respostas naturais" },
  { value: "deep_seek", name: "DeepSeek", description: "Alternativa em nuvem" },
];

/**
 * "Escolha como gerar respostas" — provider cards carry only name, one-line character
 * description, and a badge. Endpoints, streaming, and token limits never surface here;
 * see docs/onboarding.md §Configuração de IA for the full list of what's deliberately
 * left out of this screen versus developer tools.
 */
export function AiProviderScreen({ onBack, onContinue }: AiProviderScreenProps) {
  const { status, saveConfig, saveApiKey, removeApiKey } = useResponseProvider();
  const [selected, setSelected] = useState<ResponseProviderKind | null>(null);
  const active = selected ?? status?.provider ?? "ollama";

  const selectProvider = async (provider: ResponseProviderKind) => {
    setSelected(provider);
    if (provider === "ollama" && status?.provider !== "ollama") {
      // We just proved `status.provider` isn't "ollama" (or `status` hasn't loaded
      // yet), so there's no existing Ollama model choice to preserve here.
      await saveConfig({
        provider: "ollama",
        model: "llama3.1",
        baseUrl: null,
        ollamaKeepAlive: "10m",
      });
    }
  };

  return (
    <OnboardingLayout
      step={onboardingStepIndex("ai-provider")}
      totalSteps={ONBOARDING_STEPS.length}
      title="Escolha como gerar respostas"
      description="Você pode trocar isso depois, a qualquer momento."
      onBack={onBack}
      primaryLabel="Continuar"
      onPrimary={onContinue}
      primaryDisabled={!status}
    >
      <div className="flex flex-col gap-2">
        {PROVIDERS.map((provider) => (
          <ProviderOption
            key={provider.value}
            name={provider.name}
            description={provider.description}
            badge={provider.badge}
            selected={active === provider.value}
            onSelect={() => selectProvider(provider.value)}
          />
        ))}
      </div>

      {status && active === "ollama" && (
        <OllamaPanel
          status={status}
          onSave={(config) => saveConfig({ provider: "ollama", ...config })}
        />
      )}

      {status && isCloudProvider(active) && (
        <CloudProviderPanel
          provider={active}
          status={status}
          onSaveKey={async (provider, apiKey, model) => {
            await saveConfig({ provider, model, baseUrl: null, ollamaKeepAlive: null });
            await saveApiKey(provider, apiKey);
          }}
          onRemoveKey={removeApiKey}
        />
      )}
    </OnboardingLayout>
  );
}
