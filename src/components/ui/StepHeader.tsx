import type { ReactNode } from "react";

interface StepHeaderProps {
  title: string;
  description?: ReactNode;
}

/** Title + one or two lines of supporting copy. Every onboarding screen uses exactly
 * this shape — no screen gets a paragraph, per docs/onboarding.md. */
export function StepHeader({ title, description }: StepHeaderProps) {
  return (
    <div className="flex flex-col gap-1.5 text-left">
      <h1 className="text-xl font-semibold tracking-tight text-neutral-50">{title}</h1>
      {description && <p className="text-sm leading-relaxed text-neutral-400">{description}</p>}
    </div>
  );
}
