import type { ReactNode } from "react";
import { BrandMark } from "../ui/BrandMark";
import { IconButton } from "../ui/IconButton";
import { ArrowLeft } from "lucide-react";

interface AppShellProps {
  title: string;
  children: ReactNode;
  onBack?: () => void;
  headerActions?: ReactNode;
}

/** Frame for the non-onboarding, non-session "main app" screens (ready, settings,
 * developer tools) — a light header (mark + title, optional back/actions) over free
 * content. Deliberately not the same shell as the session window: this one can afford a
 * little more room to breathe. */
export function AppShell({ title, children, onBack, headerActions }: AppShellProps) {
  return (
    <div className="flex h-full min-h-screen w-full flex-col bg-app">
      <header className="flex items-center gap-3 border-b border-white/8 px-5 py-4">
        {onBack ? (
          <IconButton aria-label="Voltar" onClick={onBack}>
            <ArrowLeft className="h-4 w-4" />
          </IconButton>
        ) : (
          <BrandMark size={24} />
        )}
        <h1 className="flex-1 truncate text-sm font-semibold text-neutral-100">{title}</h1>
        {headerActions}
      </header>
      <div className="animate-fade-in flex-1 overflow-y-auto px-5 py-5">{children}</div>
    </div>
  );
}
