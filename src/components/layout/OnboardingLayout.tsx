import type { ReactNode } from "react";
import { ChevronLeft } from "lucide-react";
import { BrandMark } from "../ui/BrandMark";
import { ProgressDots } from "../ui/ProgressDots";
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
  secondaryAction?: ReactNode;
  footerNote?: ReactNode;
}

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
    <div className="flex h-full min-h-screen w-full flex-col bg-black px-8 py-7 text-neutral-100">
      <header className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <BrandMark size={18} />
          <span className="text-xs font-medium text-white/78">Helppye Setup</span>
        </div>
        <ProgressDots total={totalSteps} current={step} />
      </header>

      <main className="mx-auto flex w-full max-w-[732px] flex-1 flex-col py-12">
        <div className="mb-4 h-1 w-11 self-center rounded-full bg-white/18" />
        <p className="text-[11px] font-bold uppercase tracking-wide text-white/52">Onboarding de configuracoes</p>
        <h1 className="mt-1 text-[22px] font-bold leading-tight text-white">{title}</h1>
        {description && <p className="mt-3 max-w-[720px] text-sm leading-relaxed text-white/72">{description}</p>}

        <div className="mt-5 flex flex-col gap-2.5">{children}</div>
      </main>

      <footer className="mx-auto flex w-full max-w-[732px] items-center justify-between border-t border-white/8 pt-4">
        <div className="flex items-center gap-2">
          {onBack && (
            <GhostButton onClick={onBack}>
              <ChevronLeft className="h-4 w-4" />
              Voltar
            </GhostButton>
          )}
          {secondaryAction}
        </div>
        <div className="flex flex-col items-end gap-2">
          {primaryLabel && onPrimary && (
            <PrimaryButton onClick={onPrimary} disabled={primaryDisabled} loading={primaryLoading}>
              {primaryLabel}
            </PrimaryButton>
          )}
          {footerNote && <p className="text-right text-xs text-neutral-600">{footerNote}</p>}
        </div>
      </footer>
    </div>
  );
}
