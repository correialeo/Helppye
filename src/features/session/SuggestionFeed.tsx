import { useEffect, useRef } from "react";
import { ExchangeItem, type Exchange } from "./ExchangeItem";

interface SuggestionFeedProps {
  exchanges: Exchange[];
  onRegenerate: (turnId: number) => void;
}

/**
 * A conversa como uma sequência, não como um slot único. Cada fala da outra pessoa e sua
 * sugestão formam uma entrada própria, empilhada em ordem cronológica, mais recente
 * embaixo — uma pergunta nova aparece *abaixo* da anterior em vez de substituí-la, para
 * que o usuário nunca perca uma resposta que ainda está lendo.
 *
 * O auto-scroll só acompanha o fim quando o usuário já está no fim: se ele rolou para
 * cima para reler uma resposta anterior, uma nova entrada não arranca a tela dele.
 */
export function SuggestionFeed({ exchanges, onRegenerate }: SuggestionFeedProps) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const pinnedToBottom = useRef(true);

  const lastExchange = exchanges[exchanges.length - 1];
  const lastKey = lastExchange
    ? `${lastExchange.utteranceId}:${lastExchange.suggestion?.text.length ?? 0}`
    : "";

  useEffect(() => {
    const node = scrollRef.current;
    if (!node || !pinnedToBottom.current) return;
    node.scrollTop = node.scrollHeight;
  }, [lastKey, exchanges.length]);

  const handleScroll = () => {
    const node = scrollRef.current;
    if (!node) return;
    pinnedToBottom.current = node.scrollHeight - node.scrollTop - node.clientHeight < 32;
  };

  if (exchanges.length === 0) {
    return (
      <div className="flex flex-1 items-start overflow-hidden">
        <p className="text-sm text-neutral-500">Ouvindo a conversa...</p>
      </div>
    );
  }

  return (
    <div ref={scrollRef} onScroll={handleScroll} className="min-h-0 flex-1 overflow-y-auto">
      <div className="flex flex-col gap-5">
        {exchanges.map((exchange, index) => (
          <ExchangeItem
            key={exchange.utteranceId}
            exchange={exchange}
            isLatest={index === exchanges.length - 1}
            onRegenerate={onRegenerate}
          />
        ))}
      </div>
    </div>
  );
}
