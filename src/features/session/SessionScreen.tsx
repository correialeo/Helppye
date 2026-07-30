import { useMemo, useState } from "react";
import { SessionHeader } from "./SessionHeader";
import { TranscriptPeek } from "./TranscriptPeek";
import { SuggestionPanel } from "./SuggestionPanel";
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

function isEligible(turn: { speaker: string; source: string }): boolean {
  return turn.speaker === "other_person" && turn.source === "system_output";
}

/**
 * The compact window the whole spec revolves around. Structure mirrors
 * docs/design-system.md's mockup exactly: header (mark + status + menu), the other
 * person's last line (secondary), the suggestion (primary, fills the remaining space),
 * footer (mic/system dots + timer). Nothing here is a dashboard — there's exactly one
 * thing to look at.
 */
export function SessionScreen({ startedAt, onOpenSettings, onOpenDeveloperTools, onEndSession }: SessionScreenProps) {
  const { turns, utterances } = useConversationTimeline();
  const { suggestions } = useResponseSuggestions();
  const microphone = useAudioCapture("microphone");
  const systemOutput = useAudioCapture("system_output");
  const devMode = useOnboardingStore((s) => s.devMode);
  const [transcriptOpen, setTranscriptOpen] = useState(false);

  const lastEligibleTurn = useMemo(() => {
    const eligible = turns.filter(isEligible);
    return eligible[eligible.length - 1] ?? null;
  }, [turns]);

  const lastRemoteUtteranceText = useMemo(() => {
    const remote = utterances.filter((u) => u.speaker === "other_person");
    return remote[remote.length - 1]?.text ?? null;
  }, [utterances]);

  const suggestion = lastEligibleTurn ? suggestions[lastEligibleTurn.id] : undefined;

  const handleRegenerate = () => {
    if (lastEligibleTurn) void regenerateSuggestion(lastEligibleTurn.id);
  };

  useKeyboardShortcuts({ onToggleSession: onEndSession, onOpenSettings, onRegenerate: handleRegenerate });

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

      <div className="flex flex-1 flex-col gap-4 overflow-hidden px-4 py-4">
        <TranscriptPeek text={lastRemoteUtteranceText} />
        <div className="h-px bg-white/8" />
        <SuggestionPanel suggestion={suggestion} hasEligibleTurn={lastEligibleTurn !== null} onRegenerate={handleRegenerate} />
      </div>

      <SessionFooter microphoneStatus={microphone.status} systemStatus={systemOutput.status} startedAt={startedAt} />

      <TranscriptDrawer open={transcriptOpen} onClose={() => setTranscriptOpen(false)} utterances={utterances} />
    </div>
  );
}
