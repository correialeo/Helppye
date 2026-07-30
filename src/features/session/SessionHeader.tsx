import { useEffect, useRef, useState, type ReactNode } from "react";
import { FileText, MoreHorizontal, Settings, Terminal, X } from "lucide-react";
import { BrandMark } from "../../components/ui/BrandMark";
import { StatusIndicator } from "../../components/feedback/StatusIndicator";
import { IconButton } from "../../components/ui/IconButton";

interface SessionHeaderProps {
  listening: boolean;
  devMode: boolean;
  onOpenSettings: () => void;
  onOpenTranscript: () => void;
  onOpenDeveloperTools: () => void;
  onEndSession: () => void;
}

/** As small as the spec allows: mark, one status word, one overflow menu. Duration,
 * provider, and device names deliberately don't live here — see SessionFooter and
 * docs/design-system.md §Janela de sessão for where the "less urgent" information goes. */
export function SessionHeader({
  listening,
  devMode,
  onOpenSettings,
  onOpenTranscript,
  onOpenDeveloperTools,
  onEndSession,
}: SessionHeaderProps) {
  const [menuOpen, setMenuOpen] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!menuOpen) return;
    const handler = (event: PointerEvent) => {
      if (!containerRef.current?.contains(event.target as Node)) setMenuOpen(false);
    };
    document.addEventListener("pointerdown", handler);
    return () => document.removeEventListener("pointerdown", handler);
  }, [menuOpen]);

  const item = (icon: ReactNode, label: string, onClick: () => void) => (
    <button
      type="button"
      onClick={() => {
        setMenuOpen(false);
        onClick();
      }}
      className="flex w-full items-center gap-2.5 px-3 py-2 text-left text-sm text-neutral-300 transition-colors duration-100 hover:bg-white/6 hover:text-neutral-100"
    >
      {icon}
      {label}
    </button>
  );

  return (
    <header className="flex items-center justify-between border-b border-white/8 px-4 py-3">
      <div className="flex items-center gap-2">
        <BrandMark size={20} />
        <span className="text-sm font-medium text-neutral-200">Helppye</span>
      </div>
      <div className="flex items-center gap-3">
        <StatusIndicator label={listening ? "Ouvindo" : "Pausado"} tone={listening ? "active" : "neutral"} pulse={listening} />
        <div className="relative" ref={containerRef}>
          <IconButton aria-label="Mais opções" aria-expanded={menuOpen} onClick={() => setMenuOpen((v) => !v)}>
            <MoreHorizontal className="h-4 w-4" />
          </IconButton>
          {menuOpen && (
            <div className="animate-rise-in absolute right-0 z-20 mt-1.5 w-52 overflow-hidden rounded-lg border border-white/12 bg-surface-raised py-1 shadow-raised">
              {item(<Settings className="h-4 w-4" />, "Abrir configurações", onOpenSettings)}
              {item(<FileText className="h-4 w-4" />, "Ver transcrição", onOpenTranscript)}
              {devMode && item(<Terminal className="h-4 w-4" />, "Abrir diagnóstico", onOpenDeveloperTools)}
              <div className="my-1 border-t border-white/8" />
              {item(<X className="h-4 w-4 text-red-400" />, "Encerrar sessão", onEndSession)}
            </div>
          )}
        </div>
      </div>
    </header>
  );
}
