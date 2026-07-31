import { useMemo, useState } from "react";
import { SessionHeader } from "./SessionHeader";
import { SuggestionFeed } from "./SuggestionFeed";
import type { Exchange } from "./ExchangeItem";
import { SessionFooter } from "./SessionFooter";
import { TranscriptDrawer } from "./TranscriptDrawer";
import { useConversationTimeline } from "../../hooks/useConversationTimeline";
import { useResponseSuggestions } from "../../hooks/useResponseSuggestions";
import { useAudioCapture } from "../../hooks/useAudioCapture";
import { useOnboardingStore } from "../../stores/useOnboardingStore";
import { regenerateSuggestion } from "../../services/conversationService";
import { useKeyboardShortcuts } from "../../hooks/useKeyboardShortcuts";

interface SessionScreenProps {
  startedAt: number;
  onOpenSettings: () => void;
  onOpenDeveloperTools: () => void;
  onEndSession: () => void;
}

/** Mesma regra de elegibilidade do backend (`response_provider::engine::is_eligible_turn`):
 * só a fala da outra pessoa, vinda da saída do sistema, pede uma sugestão. */
function isEligible(item: { speaker: string; source: string }): boolean {
  return item.speaker === "other_person" && item.source === "system_output";
}

/**
 * The compact window the whole spec revolves around: header (mark + status + menu), the
 * conversation feed (primary, fills the remaining space), footer (mic/system dots +
 * timer). Nothing here is a dashboard — there's exactly one thing to look at.
 *
 * O feed é indexado por **utterance**, não por turno. Um `ConversationTurn` agrupa tudo
 * que a outra pessoa falou enquanto manteve a palavra e pode conter várias perguntas;
 * mostrando só o último turno elegível, a resposta à segunda pergunta substituía a
 * resposta à primeira no lugar. Cada fala vira uma entrada própria, e o que é novo entra
 * abaixo — nada é sobrescrito.
 */
export function SessionScreen({ startedAt, onOpenSettings, onOpenDeveloperTools, onEndSession }: SessionScreenProps) {
  const { turns, utterances } = useConversationTimeline();
  const { suggestions } = useResponseSuggestions();
  const microphone = useAudioCapture("microphone");
  const systemOutput = useAudioCapture("system_output");
  const devMode = useOnboardingStore((s) => s.devMode);
  const [transcriptOpen, setTranscriptOpen] = useState(false);

  const exchanges = useMemo<Exchange[]>(() => {
    const turnOf = new Map<number, number>();
    for (const turn of turns) {
      for (const utteranceId of turn.utterances) {
        turnOf.set(utteranceId, turn.id);
      }
    }
    return utterances
      .filter((u) => isEligible(u) && u.text.trim().length > 0)
      .map((u) => ({
        utteranceId: u.id,
        // O turno vem da timeline; enquanto ele ainda não listou esta fala, a própria
        // sugestão já carrega o turno de origem (o backend emite os dois juntos).
        turnId: turnOf.get(u.id) ?? suggestions[u.id]?.turnId ?? -1,
        question: u.text,
        suggestion: suggestions[u.id],
      }));
  }, [turns, utterances, suggestions]);

  const handleRegenerate = (turnId: number) => {
    if (turnId >= 0) void regenerateSuggestion(turnId);
  };

  const latestTurnId = exchanges[exchanges.length - 1]?.turnId ?? -1;

  useKeyboardShortcuts({
    onToggleSession: onEndSession,
    onOpenSettings,
    onRegenerate: () => handleRegenerate(latestTurnId),
  });

  return (
    <div className="flex h-full min-h-screen w-full flex-col bg-app">
      <SessionHeader
        listening={microphone.status.kind === "capturing" || systemOutput.status.kind === "capturing"}
        devMode={devMode}
        onOpenSettings={onOpenSettings}
        onOpenTranscript={() => setTranscriptOpen(true)}
        onOpenDeveloperTools={onOpenDeveloperTools}
        onEndSession={onEndSession}
      />

      <div className="flex flex-1 flex-col overflow-hidden px-4 py-4">
        <SuggestionFeed exchanges={exchanges} onRegenerate={handleRegenerate} />
      </div>

      <SessionFooter microphoneStatus={microphone.status} systemStatus={systemOutput.status} startedAt={startedAt} />

      <TranscriptDrawer open={transcriptOpen} onClose={() => setTranscriptOpen(false)} utterances={utterances} />
    </div>
  );
}
