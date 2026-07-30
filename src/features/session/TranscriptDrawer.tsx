import { Dialog } from "../../components/ui/Dialog";
import { formatTimelineTime } from "../../utils/format";
import type { ConversationUtterance } from "../../types/conversation";

interface TranscriptDrawerProps {
  open: boolean;
  onClose: () => void;
  utterances: ConversationUtterance[];
}

/** The full history, available on demand — never on screen by default (see docs/design-
 * system.md §Complexidade ocultada). Plain speaker + text + time, nothing else: no
 * segment IDs, no turn IDs, no internal state. */
export function TranscriptDrawer({ open, onClose, utterances }: TranscriptDrawerProps) {
  return (
    <Dialog open={open} onClose={onClose} title="Transcrição">
      {utterances.length === 0 ? (
        <p className="text-sm text-neutral-500">Nada foi transcrito ainda.</p>
      ) : (
        <ol className="flex max-h-[60vh] flex-col gap-3 overflow-y-auto">
          {utterances.map((utterance) => (
            <li key={utterance.id} className="grid grid-cols-[3.5rem_1fr] gap-3 text-sm">
              <span className="pt-0.5 text-xs text-neutral-600">{formatTimelineTime(utterance.started_at)}</span>
              <div>
                <p className="text-xs font-medium text-neutral-500">
                  {utterance.speaker === "user" ? "Você" : "Outra pessoa"}
                </p>
                <p className="whitespace-pre-wrap text-neutral-200">{utterance.text || "..."}</p>
              </div>
            </li>
          ))}
        </ol>
      )}
    </Dialog>
  );
}
