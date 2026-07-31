import type { ReactNode } from "react";
import { ArrowLeft } from "lucide-react";
import { BrandMark } from "../ui/BrandMark";
import { IconButton } from "../ui/IconButton";

interface AppShellProps {
  title: string;
  children: ReactNode;
  onBack?: () => void;
  headerActions?: ReactNode;
}

export function AppShell({ title, children, onBack, headerActions }: AppShellProps) {
  return (
    <div className="flex h-full min-h-screen w-full flex-col bg-black text-neutral-100">
      <header className="flex h-11 items-center gap-3 border-b border-white/8 px-4">
        {onBack ? (
          <IconButton aria-label="Voltar" onClick={onBack} className="h-7 w-7">
            <ArrowLeft className="h-4 w-4" />
          </IconButton>
        ) : (
          <BrandMark size={20} />
        )}
        <h1 className="flex-1 truncate text-sm font-semibold text-white/86">{title}</h1>
        {headerActions}
      </header>
      <div className="flex-1 overflow-y-auto px-5 py-5">
        <div className="mx-auto w-full max-w-[732px]">{children}</div>
      </div>
    </div>
  );
}
