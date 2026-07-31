import type { ReactNode } from "react";
import { Globe, Mic, MonitorSpeaker, Pencil, Sparkles } from "lucide-react";
import { OnboardingLayout } from "../../components/layout/OnboardingLayout";
import { IconButton } from "../../components/ui/IconButton";
import { ONBOARDING_STEPS, onboardingStepIndex, type AppScreen } from "../../app/appFlow";
import { useAudioCaptureStore } from "../../stores/useAudioCaptureStore";
import { useResponseProvider } from "../../hooks/useResponseProvider";
import type { ResponseProviderKind } from "../../types/responseProvider";

/** Um nome por provedor conhecido. `Record` completo de propósito: o typecheck passa a
 * cobrar esta linha quando um provedor novo é adicionado no backend, em vez de deixar o
 * resumo de onboarding exibir `undefined`. */
const PROVIDER_NAMES: Record<ResponseProviderKind, string> = {
  ollama: "Ollama",
  lm_studio: "LM Studio",
  open_ai: "OpenAI",
  anthropic: "Anthropic",
  deep_seek: "DeepSeek",
  open_router: "OpenRouter",
  custom_open_ai_compatible: "Endpoint personalizado",
};

interface OnboardingReviewScreenProps {
  onBack: () => void;
  onContinue: () => void;
  onEdit: (screen: AppScreen) => void;
}

function ReviewRow({
  icon,
  label,
  onEdit,
}: {
  icon: ReactNode;
  label: string;
  onEdit: () => void;
}) {
  return (
    <div className="flex items-center justify-between gap-3 rounded-lg border border-white/10 bg-surface px-3.5 py-2.5">
      <span className="flex items-center gap-2.5 text-sm text-neutral-200">
        <span className="text-neutral-500">{icon}</span>
        {label}
      </span>
      <IconButton aria-label={`Editar ${label}`} onClick={onEdit}>
        <Pencil className="h-3.5 w-3.5" />
      </IconButton>
    </div>
  );
}

/** A short, scannable list — not a series of large summary cards. Each row edits its own
 * step directly, so fixing one thing never means walking the whole onboarding again. */
export function OnboardingReviewScreen({ onBack, onContinue, onEdit }: OnboardingReviewScreenProps) {
  const microphone = useAudioCaptureStore((s) => s.microphone);
  const systemOutput = useAudioCaptureStore((s) => s.system_output);
  const { status } = useResponseProvider();

  const microphoneName = microphone.devices.find((d) => d.id === microphone.selectedId)?.name ?? "Padrão do sistema";
  const outputName = systemOutput.devices.find((d) => d.id === systemOutput.selectedId)?.name ?? "Padrão do sistema";
  const providerLabel = status && `${PROVIDER_NAMES[status.provider]} · ${status.model}`;

  return (
    <OnboardingLayout
      step={onboardingStepIndex("onboarding-review")}
      totalSteps={ONBOARDING_STEPS.length}
      title="Está tudo certo"
      onBack={onBack}
      primaryLabel="Continuar"
      onPrimary={onContinue}
    >
      <div className="flex flex-col gap-2">
        <ReviewRow icon={<Globe className="h-4 w-4" />} label="Português" onEdit={() => onEdit("language")} />
        <ReviewRow icon={<Mic className="h-4 w-4" />} label={microphoneName} onEdit={() => onEdit("audio-setup")} />
        <ReviewRow
          icon={<MonitorSpeaker className="h-4 w-4" />}
          label={outputName}
          onEdit={() => onEdit("audio-setup")}
        />
        {providerLabel && (
          <ReviewRow icon={<Sparkles className="h-4 w-4" />} label={providerLabel} onEdit={() => onEdit("ai-provider")} />
        )}
      </div>
    </OnboardingLayout>
  );
}
