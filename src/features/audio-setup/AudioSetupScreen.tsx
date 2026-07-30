import { Mic, MonitorSpeaker } from "lucide-react";
import { OnboardingLayout } from "../../components/layout/OnboardingLayout";
import { ONBOARDING_STEPS, onboardingStepIndex } from "../../app/appFlow";
import { useModelStatus } from "../../hooks/useModelStatus";
import { ModelPrepareStep } from "./ModelPrepareStep";
import { DeviceTestBlock } from "./DeviceTestBlock";

interface AudioSetupScreenProps {
  onBack: () => void;
  onContinue: () => void;
}

/** Guided test, not a device configuration form: fires up both sources (already granted
 * on the previous screen) and shows their live level so the user can *see* the pipeline
 * working, per docs/onboarding.md §Áudio. Blocked behind ModelPrepareStep until local
 * transcription is actually ready — no point testing audio the app can't transcribe yet. */
export function AudioSetupScreen({ onBack, onContinue }: AudioSetupScreenProps) {
  const model = useModelStatus();

  if (model.status?.state.state !== "ready") {
    return <ModelPrepareStep model={model} />;
  }

  return (
    <OnboardingLayout
      step={onboardingStepIndex("audio-setup")}
      totalSteps={ONBOARDING_STEPS.length}
      title="Vamos testar o áudio"
      description="Fale alguma coisa e reproduza um som no computador."
      onBack={onBack}
      primaryLabel="Continuar"
      onPrimary={onContinue}
    >
      <DeviceTestBlock icon={<Mic className="h-4 w-4" />} title="Microfone" source="microphone" />
      <DeviceTestBlock icon={<MonitorSpeaker className="h-4 w-4" />} title="Áudio do computador" source="system_output" />
    </OnboardingLayout>
  );
}
