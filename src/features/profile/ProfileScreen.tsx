import { useState } from "react";
import { OnboardingLayout } from "../../components/layout/OnboardingLayout";
import { TextInput } from "../../components/ui/TextInput";
import { GhostButton } from "../../components/ui/GhostButton";
import { ONBOARDING_STEPS, onboardingStepIndex } from "../../app/appFlow";
import { useOnboardingStore } from "../../stores/useOnboardingStore";

interface ProfileScreenProps {
  onBack: () => void;
  onContinue: () => void;
}

export function ProfileScreen({ onBack, onContinue }: ProfileScreenProps) {
  const userName = useOnboardingStore((s) => s.userName);
  const setUserName = useOnboardingStore((s) => s.setUserName);
  const [draft, setDraft] = useState(userName);

  const continueWithName = () => {
    setUserName(draft);
    onContinue();
  };

  const skip = () => {
    setUserName("");
    onContinue();
  };

  return (
    <OnboardingLayout
      step={onboardingStepIndex("profile")}
      totalSteps={ONBOARDING_STEPS.length}
      title="Como podemos chamar você?"
      description="Isso ajuda o Helppye a personalizar sua experiência."
      onBack={onBack}
      secondaryAction={<GhostButton onClick={skip}>Pular</GhostButton>}
      primaryLabel="Continuar"
      onPrimary={continueWithName}
    >
      <TextInput
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        placeholder="Seu nome"
        autoFocus
        onKeyDown={(e) => e.key === "Enter" && continueWithName()}
      />
    </OnboardingLayout>
  );
}
