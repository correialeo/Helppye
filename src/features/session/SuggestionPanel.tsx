import { useEffect, useState } from "react";
import { Copy, Eye, RefreshCw } from "lucide-react";
import { GhostButton } from "../../components/ui/GhostButton";
import { Kbd } from "../../components/ui/Kbd";
import type { SuggestionState } from "./responseSuggestionViewModel";

interface SuggestionPanelProps {
  suggestion: SuggestionState | undefined;
  hasEligibleTurn: boolean;
  onRegenerate: () => void;
}

/**
 * Editorial content, not a chat bubble: no avatar, no model name, no "AI:" prefix — just
 * well-set text. This is the single most important element on screen (see
 * docs/design-system.md §Janela de sessão), so every other choice here is about keeping
 * attention on the words: a discrete shimmer while preparing, a blinking cursor while
 * streaming, and — per docs/session-experience.md — the previous answer never disappears
 * before the next one has something real to replace it with.
 */
export function SuggestionPanel({ suggestion, hasEligibleTurn, onRegenerate }: SuggestionPanelProps) {
  const [hidden, setHidden] = useState(false);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    setHidden(false);
  }, [suggestion?.generationId]);

  const displayText = suggestion?.text || suggestion?.previousText;
  const isActive = suggestion?.status === "preparing" || suggestion?.status === "streaming";

  const copy = async () => {
    if (!displayText) return;
    await navigator.clipboard.writeText(displayText);
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1500);
  };

  if (hidden) {
    return (
      <div className="flex flex-1 items-center justify-between rounded-lg border border-white/8 bg-white/3 px-4 py-3">
        <p className="text-sm text-neutral-500">Sugestão oculta</p>
        <GhostButton onClick={() => setHidden(false)}>
          <Eye className="h-3.5 w-3.5" /> Mostrar
        </GhostButton>
      </div>
    );
  }

  return (
    <div className="flex flex-1 flex-col gap-3 overflow-hidden">
      <div className="min-h-0 flex-1 overflow-y-auto">
        {!hasEligibleTurn && !displayText ? (
          <p className="text-sm text-neutral-500">Ouvindo a conversa...</p>
        ) : displayText ? (
          <p className="whitespace-pre-wrap text-[15px] leading-relaxed text-neutral-100">
            {displayText}
            {suggestion?.status === "streaming" && (
              <span className="ml-0.5 inline-block h-[1em] w-[2px] translate-y-[2px] animate-pulse-soft bg-brand-400" />
            )}
          </p>
        ) : suggestion?.status === "preparing" ? (
          <p className="shimmer-text text-sm font-medium">Preparando uma sugestão...</p>
        ) : suggestion?.status === "error" ? (
          <div className="flex flex-col gap-2">
            <p className="text-sm text-neutral-400">Não foi possível gerar a sugestão.</p>
            <GhostButton className="w-fit" onClick={onRegenerate}>
              <RefreshCw className="h-3.5 w-3.5" /> Tentar novamente
            </GhostButton>
          </div>
        ) : (
          <p className="text-sm text-neutral-600">Nenhuma nova sugestão</p>
        )}
      </div>

      {displayText && !isActive && (
        <div className="flex items-center gap-1 border-t border-white/8 pt-2.5">
          <GhostButton onClick={copy}>
            <Copy className="h-3.5 w-3.5" /> {copied ? "Copiado" : "Copiar"}
          </GhostButton>
          <GhostButton onClick={onRegenerate}>
            <RefreshCw className="h-3.5 w-3.5" /> Regenerar
          </GhostButton>
          <GhostButton onClick={() => setHidden(true)}>
            <Eye className="h-3.5 w-3.5" /> Ocultar
          </GhostButton>
          <span className="ml-auto opacity-60">
            <Kbd keys={["mod", "shift", "enter"]} />
          </span>
        </div>
      )}
    </div>
  );
}
