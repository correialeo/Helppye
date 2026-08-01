import { useEffect, useRef, useState } from "react";
import { onConversationTimelineEvent, onResponseSuggestionEvent } from "../services/conversationService";
import {
  applyResponseSuggestionDiagnostics,
  applyResponseSuggestionEvent,
  type ResponseSuggestionDiagnostics,
  type SuggestionState,
} from "../features/session/responseSuggestionViewModel";

/** Live response-suggestion state per **utterance** (visible text) plus, separately, the
 * raw diagnostics keyed by turn (developer tools only) — see
 * responseSuggestionViewModel.ts for why these are two different reducers, and why the
 * visible state cannot be keyed by turn. */
export function useResponseSuggestions() {
  const [suggestions, setSuggestions] = useState<Record<number, SuggestionState>>({});
  const [diagnostics, setDiagnostics] = useState<Record<number, ResponseSuggestionDiagnostics>>({});
  const activeSessionId = useRef<number>();

  useEffect(() => {
    const unlistenPromise = onResponseSuggestionEvent((event) => {
      if (event.session_id !== activeSessionId.current) return;
      setSuggestions((current) =>
        applyResponseSuggestionEvent(current, event, activeSessionId.current),
      );
      setDiagnostics((current) => applyResponseSuggestionDiagnostics(current, event));
    });
    return () => {
      unlistenPromise.then((unlisten) => unlisten());
    };
  }, []);

  // Fronteira de sessão: o backend já garante que nenhuma sugestão da sessão encerrada
  // volta a ser emitida — isto só evita que a última sugestão continue em memória aqui
  // depois que a conversa que a originou deixou de existir.
  useEffect(() => {
    const unlistenPromise = onConversationTimelineEvent((event) => {
      if (event.type === "session_ended" || event.type === "session_started") {
        activeSessionId.current =
          event.type === "session_started" ? event.session_id : undefined;
        setSuggestions({});
        setDiagnostics({});
      }
    });
    return () => {
      unlistenPromise.then((unlisten) => unlisten());
    };
  }, []);

  return { suggestions, diagnostics };
}
