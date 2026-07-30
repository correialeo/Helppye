import { Check } from "lucide-react";
import { OnboardingLayout } from "../../components/layout/OnboardingLayout";
import { ONBOARDING_STEPS, onboardingStepIndex } from "../../app/appFlow";

interface LanguageScreenProps {
  onBack: () => void;
  onContinue: () => void;
}

/** Only one option exists today (pt-BR) — shown as a real selectable item, not a
 * `<select>` with a single entry, so the screen still reads as a deliberate choice
 * rather than a dead-end form control. New languages slot into the same list later
 * (`useOnboardingStore.language` already models it as an extensible union). */
export function LanguageScreen({ onBack, onContinue }: LanguageScreenProps) {
  return (
    <OnboardingLayout
      step={onboardingStepIndex("language")}
      totalSteps={ONBOARDING_STEPS.length}
      title="Qual idioma você usa nas conversas?"
      onBack={onBack}
      primaryLabel="Continuar"
      onPrimary={onContinue}
    >
      <div
        className="flex w-full items-center justify-between rounded-lg border border-brand-400/70 bg-brand-500/8 px-4 py-3 text-left"
        role="radio"
        aria-checked="true"
        tabIndex={0}
      >
        <div className="flex flex-col">
          <span className="text-sm font-medium text-neutral-100">Português</span>
          <span className="text-xs text-neutral-500">Brasil</span>
        </div>
        <Check className="h-4 w-4 text-brand-400" aria-hidden="true" />
      </div>
      <p className="text-xs text-neutral-600">Mais idiomas chegam em versões futuras.</p>
    </OnboardingLayout>
  );
}
