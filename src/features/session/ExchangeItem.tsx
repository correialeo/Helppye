import { useState } from "react";
import { Copy, Eye, RefreshCw } from "lucide-react";
import { GhostButton } from "../../components/ui/GhostButton";
import { Kbd } from "../../components/ui/Kbd";
import type { SuggestionState } from "./responseSuggestionViewModel";

export interface Exchange {
  utteranceId: number;
  turnId: number;
  /** O que a outra pessoa falou — a pergunta a que a sugestão responde. */
  question: string;
  suggestion: SuggestionState | undefined;
}

interface ExchangeItemProps {
  exchange: Exchange;
  /** O atalho de regenerar age sobre o par mais recente — só ele mostra a dica de teclado. */
  isLatest: boolean;
  onRegenerate: (turnId: number) => void;
}

/**
 * Um par fala-da-outra-pessoa → sugestão. Cada par é uma entrada própria e imutável no
 * feed: uma pergunta nova entra **abaixo**, nunca por cima de uma resposta que o usuário
 * ainda pode estar lendo. A hierarquia visual do mockup (docs/design-system.md) é
 * preservada dentro do par — a fala é contexto secundário, a sugestão é o conteúdo.
 */
export function ExchangeItem({ exchange, isLatest, onRegenerate }: ExchangeItemProps) {
  const [hidden, setHidden] = useState(false);
  const [copied, setCopied] = useState(false);

  const { suggestion } = exchange;
  const isActive = suggestion?.status === "preparing" || suggestion?.status === "streaming";
  // `regenerateSuggestion` é um comando por turno; sem turno conhecido não há o que pedir.
  const canRegenerate = exchange.turnId >= 0;

  const copy = async () => {
    if (!suggestion?.text) return;
    await navigator.clipboard.writeText(suggestion.text);
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1500);
  };

  return (
    <div className="flex flex-col gap-2">
      <div className="flex flex-col gap-0.5">
        <p className="text-xs font-medium text-neutral-500">Outra pessoa</p>
        <p className="text-sm leading-snug text-neutral-400">{exchange.question}</p>
      </div>

      {hidden ? (
        <div className="flex items-center justify-between rounded-lg border border-white/8 bg-white/3 px-4 py-3">
          <p className="text-sm text-neutral-500">Sugestão oculta</p>
          <GhostButton onClick={() => setHidden(false)}>
            <Eye className="h-3.5 w-3.5" /> Mostrar
          </GhostButton>
        </div>
      ) : suggestion?.text ? (
        <p className="whitespace-pre-wrap text-[15px] leading-relaxed text-neutral-100">
          {suggestion.text}
          {suggestion.status === "streaming" && (
            <span className="ml-0.5 inline-block h-[1em] w-[2px] translate-y-[2px] animate-pulse-soft bg-brand-400" />
          )}
        </p>
      ) : suggestion?.status === "preparing" ? (
        <p className="shimmer-text text-sm font-medium">Preparando uma sugestão...</p>
      ) : suggestion?.status === "error" ? (
        <div className="flex flex-col gap-2">
          <p className="text-sm text-neutral-400">Não foi possível gerar a sugestão.</p>
          {canRegenerate && (
            <GhostButton className="w-fit" onClick={() => onRegenerate(exchange.turnId)}>
              <RefreshCw className="h-3.5 w-3.5" /> Tentar novamente
            </GhostButton>
          )}
        </div>
      ) : suggestion?.status === "skipped" || suggestion?.status === "completed_empty" ? (
        <p className="text-sm text-neutral-600">Nenhuma sugestão para esta fala</p>
      ) : null}

      {suggestion?.text && !isActive && !hidden && (
        <div className="flex items-center gap-1 pt-0.5">
          <GhostButton onClick={copy}>
            <Copy className="h-3.5 w-3.5" /> {copied ? "Copiado" : "Copiar"}
          </GhostButton>
          {canRegenerate && (
            <GhostButton onClick={() => onRegenerate(exchange.turnId)}>
              <RefreshCw className="h-3.5 w-3.5" /> Regenerar
            </GhostButton>
          )}
          <GhostButton onClick={() => setHidden(true)}>
            <Eye className="h-3.5 w-3.5" /> Ocultar
          </GhostButton>
          {isLatest && (
            <span className="ml-auto opacity-60">
              <Kbd keys={["mod", "shift", "enter"]} />
            </span>
          )}
        </div>
      )}
    </div>
  );
}
