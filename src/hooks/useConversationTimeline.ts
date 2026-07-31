import { useEffect, useState } from "react";
import { getConversationTimelineSnapshot, onConversationTimelineEvent } from "../services/conversationService";
import type { ConversationTurn, ConversationUtterance } from "../types/conversation";

function sortByStart<T extends { started_at: number; ended_at: number; id: number }>(items: T[]): T[] {
  return [...items].sort((a, b) => a.started_at - b.started_at || a.ended_at - b.ended_at || a.id - b.id);
}

/** Live Conversation Timeline — turns/utterances, kept in sync via
 * `conversation://timeline-event`. The one hook both the session screen (last remote
 * utterance) and the transcript drawer / developer tools (full history) read from. */
export function useConversationTimeline() {
  const [turns, setTurns] = useState<ConversationTurn[]>([]);
  const [utterances, setUtterances] = useState<ConversationUtterance[]>([]);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    getConversationTimelineSnapshot()
      .then((snapshot) => {
        setTurns(sortByStart(snapshot.turns));
        setUtterances(sortByStart(snapshot.utterances));
        setError(null);
      })
      .catch((e) => setError(String(e)));
  }, []);

  useEffect(() => {
    const unlistenPromise = onConversationTimelineEvent((event) => {
      if (event.type === "turn_started") {
        setTurns((current) => {
          const next: ConversationTurn = {
            id: event.turn_id,
            speaker: event.speaker,
            source: event.source,
            text: "",
            utterances: [],
            started_at: event.started_at,
            ended_at: event.started_at,
            finalized_at: null,
          };
          return sortByStart([...current.filter((t) => t.id !== next.id), next]);
        });
        return;
      }

      if (event.type === "turn_updated" || event.type === "turn_finalized") {
        setTurns((current) => sortByStart([...current.filter((t) => t.id !== event.turn.id), event.turn]));
        return;
      }

      if (event.type === "utterance_started") {
        setUtterances((current) => {
          const next: ConversationUtterance = {
            id: event.utterance_id,
            speaker: event.speaker,
            source: event.source,
            text: "",
            segments: [],
            started_at: event.started_at,
            ended_at: event.started_at,
            finalized_at: null,
            revision: 1,
          };
          return sortByStart([...current.filter((u) => u.id !== next.id), next]);
        });
        return;
      }

      if (event.type === "utterance_updated") {
        setUtterances((current) => {
          const existing = current.find((u) => u.id === event.utterance_id);
          const next: ConversationUtterance = {
            id: event.utterance_id,
            speaker: existing?.speaker ?? event.speaker,
            source: existing?.source ?? event.source,
            text: event.text,
            segments: event.segments,
            started_at: existing?.started_at ?? event.started_at,
            ended_at: event.ended_at,
            finalized_at: existing?.finalized_at ?? null,
            revision: existing?.revision ?? 1,
          };
          return sortByStart([...current.filter((u) => u.id !== next.id), next]);
        });
        return;
      }

      if (event.type === "utterance_finalized") {
        // Carries the full ConversationUtterance already.
        setUtterances((current) =>
          sortByStart([...current.filter((u) => u.id !== event.utterance.id), event.utterance]),
        );
        return;
      }

      // A sessão anterior deixou de existir no backend (turnos, utterances e contexto
      // apagados). A tela acompanha em vez de continuar mostrando uma conversa que já não
      // é mais consultável.
      //
      // Explícito de propósito, e não um `else` que pega tudo que não casou acima: como
      // catch-all, qualquer evento novo ou desconhecido no canal apagaria a timeline
      // inteira da tela — a janela de sessão ficaria presa em "Ouvindo a conversa...", que
      // é exatamente o estado exibido quando não existe turno elegível.
      if (event.type === "session_ended" || event.type === "session_started") {
        setTurns([]);
        setUtterances([]);
      }
    });
    return () => {
      unlistenPromise.then((unlisten) => unlisten());
    };
  }, []);

  return { turns, utterances, error };
}
