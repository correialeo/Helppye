import { useEffect, useState } from "react";
import { onResponseSuggestionEvent } from "../services/conversationService";
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

  return { suggestions, diagnostics };
}
