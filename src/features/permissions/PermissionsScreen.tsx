import { useState, type ReactNode } from "react";
import { Check, Loader2, Mic, MonitorSpeaker } from "lucide-react";
import { OnboardingLayout } from "../../components/layout/OnboardingLayout";
import { SecondaryButton } from "../../components/ui/SecondaryButton";
import { InlineNotice } from "../../components/ui/InlineNotice";
import { ONBOARDING_STEPS, onboardingStepIndex } from "../../app/appFlow";
import { useAudioCapture } from "../../hooks/useAudioCapture";
import type { AudioSourceKind } from "../../types/audio";

interface PermissionsScreenProps {
  onBack: () => void;
  onContinue: () => void;
}

const PLATFORM_DETAILS: Record<AudioSourceKind, string> = {
  microphone:
    "No Windows e no macOS, o sistema pode pedir sua confirmação na primeira vez que um app acessa o microfone.",
  system_output:
    "No Windows, a captura de saída usa WASAPI Loopback. No Linux, PipeWire. Alguns sistemas pedem permissão adicional para gravação de tela/áudio do sistema.",
};

function PermissionRow({
  icon,
  title,
  description,
  source,
}: {
  icon: ReactNode;
  title: string;
  description: string;
  source: AudioSourceKind;
}) {
  const { status, start } = useAudioCapture(source);
  const [detailsOpen, setDetailsOpen] = useState(false);
  const granted = status.kind === "capturing";
  const pending = status.kind === "switching";

  return (
    <div className="flex flex-col gap-2 rounded-lg border border-white/10 bg-surface px-4 py-3.5">
      <div className="flex items-center gap-3">
        <span className="flex h-9 w-9 flex-shrink-0 items-center justify-center rounded-md bg-white/6 text-neutral-300">
          {icon}
        </span>
        <div className="min-w-0 flex-1">
          <p className="text-sm font-medium text-neutral-100">{title}</p>
          <p className="text-xs leading-relaxed text-neutral-500">{description}</p>
        </div>
        {granted ? (
          <span className="flex items-center gap-1 text-xs font-medium text-emerald-400">
            <Check className="h-3.5 w-3.5" /> Permitido
          </span>
        ) : (
          <SecondaryButton className="px-3 py-1.5 text-xs" onClick={start} disabled={pending}>
            {pending ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : "Permitir"}
          </SecondaryButton>
        )}
      </div>

      {status.kind === "error" && (
        <InlineNotice tone="error">
          Não conseguimos acessar este dispositivo.{" "}
          <button type="button" className="underline underline-offset-2" onClick={() => setDetailsOpen((v) => !v)}>
            Saiba mais
          </button>
          {detailsOpen && <p className="mt-1.5 text-xs text-red-300/80">{PLATFORM_DETAILS[source]}</p>}
        </InlineNotice>
      )}
    </div>
  );
}

/** Attempting to start capture *is* the permission check — there's no separate OS
 * permission-probe command, and trying to start is exactly what would surface a
 * platform permission prompt on a real device. See docs/onboarding.md §Permissões. */
export function PermissionsScreen({ onBack, onContinue }: PermissionsScreenProps) {
  return (
    <OnboardingLayout
      step={onboardingStepIndex("permissions")}
      totalSteps={ONBOARDING_STEPS.length}
      title="Precisamos ouvir a conversa"
      description="O Helppye usa duas fontes para entender o que está sendo dito."
      onBack={onBack}
      primaryLabel="Continuar"
      onPrimary={onContinue}
    >
      <PermissionRow
        icon={<Mic className="h-4 w-4" />}
        title="Sua voz"
        description="Usamos o microfone para entender o que você responde."
        source="microphone"
      />
      <PermissionRow
        icon={<MonitorSpeaker className="h-4 w-4" />}
        title="A outra pessoa"
        description="Capturamos o áudio reproduzido na chamada."
        source="system_output"
      />
    </OnboardingLayout>
  );
}
