import { useEffect, useState } from "react";
import { onConversationTimelineEvent, onResponseSuggestionEvent } from "../services/conversationService";
import {
  applyResponseSuggestionDiagnostics,
  applyResponseSuggestionEvent,
  type ResponseSuggestionDiagnostics,
  type SuggestionState,
} from "../features/session/responseSuggestionViewModel";

/** Live response-suggestion state per turn (visible text) plus, separately, the raw
 * per-generation diagnostics (developer tools only) — see
 * responseSuggestionViewModel.ts for why these are two different reducers. */
export function useResponseSuggestions() {
  const [suggestions, setSuggestions] = useState<Record<number, SuggestionState>>({});
  const [diagnostics, setDiagnostics] = useState<Record<number, ResponseSuggestionDiagnostics>>({});

  useEffect(() => {
    const unlistenPromise = onResponseSuggestionEvent((event) => {
      setSuggestions((current) => applyResponseSuggestionEvent(current, event));
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
