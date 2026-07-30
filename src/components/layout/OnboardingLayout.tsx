import type { ReactNode } from "react";
import { BrandMark } from "../ui/BrandMark";
import { ProgressDots } from "../ui/ProgressDots";
import { StepHeader } from "../ui/StepHeader";
import { GhostButton } from "../ui/GhostButton";
import { PrimaryButton } from "../ui/PrimaryButton";

interface OnboardingLayoutProps {
  step: number;
  totalSteps: number;
  title: string;
  description?: ReactNode;
  children: ReactNode;
  onBack?: () => void;
  primaryLabel?: string;
  onPrimary?: () => void;
  primaryDisabled?: boolean;
  primaryLoading?: boolean;
  /** An extra discrete action next to "Voltar" — e.g. "Pular" on the name step. */
  secondaryAction?: ReactNode;
  footerNote?: ReactNode;
}

/**
 * The one shell every onboarding screen renders through — title/description, central
 * content, and a footer with at most one dominant action (PrimaryButton) plus quiet
 * secondary ones (GhostButton). Keeping this in a single layout is what makes "one
 * primary action per screen" (docs/design-system.md §Critério de avaliação) hold
 * automatically instead of being re-litigated on every screen.
 */
export function OnboardingLayout({
  step,
  totalSteps,
  title,
  description,
  children,
  onBack,
  primaryLabel,
  onPrimary,
  primaryDisabled,
  primaryLoading,
  secondaryAction,
  footerNote,
}: OnboardingLayoutProps) {
  return (
    <div className="flex h-full min-h-screen w-full flex-col bg-app px-6 py-6">
      <header className="flex items-center justify-between">
        <BrandMark size={26} />
        <ProgressDots total={totalSteps} current={step} />
      </header>

      <div className="mx-auto flex w-full max-w-sm flex-1 flex-col justify-center gap-6 py-8">
        <StepHeader title={title} description={description} />
        <div className="animate-rise-in flex flex-col gap-4">{children}</div>
      </div>

      <footer className="mx-auto flex w-full max-w-sm flex-col gap-2">
        <div className="flex items-center justify-between gap-3">
          <div className="flex items-center gap-1">
            {onBack && <GhostButton onClick={onBack}>Voltar</GhostButton>}
            {secondaryAction}
          </div>
          {primaryLabel && (
            <PrimaryButton onClick={onPrimary} disabled={primaryDisabled} loading={primaryLoading}>
              {primaryLabel}
            </PrimaryButton>
          )}
        </div>
        {footerNote && <p className="text-center text-xs text-neutral-600">{footerNote}</p>}
      </footer>
    </div>
  );
}
