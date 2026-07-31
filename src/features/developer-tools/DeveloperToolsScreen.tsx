import { useCallback, useEffect, useState } from "react";
import { Check, Copy } from "lucide-react";
import { AppShell } from "../../components/layout/AppShell";
import { SecondaryButton } from "../../components/ui/SecondaryButton";
import { useConversationTimeline } from "../../hooks/useConversationTimeline";
import { useResponseSuggestions } from "../../hooks/useResponseSuggestions";
import { getUtteranceGapMs, setUtteranceGapMs } from "../../services/conversationService";

const UTTERANCE_GAP_PRESETS_MS = [1200, 1500, 1800, 2200];

function UtteranceGapControl() {
  const [gapMs, setGapMs] = useState<number | null>(null);

  const refresh = useCallback(() => {
    getUtteranceGapMs().then(setGapMs).catch(() => {});
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  return (
    <div className="flex flex-col gap-2">
      <p className="text-xs text-neutral-500">
        same_speaker_utterance_gap_ms — atual: {gapMs === null ? "..." : `${gapMs} ms`}. Valores baixos disparam a
        sugestão mais rápido, mas arriscam responder no meio de uma pergunta ainda incompleta.
      </p>
      <div className="flex flex-wrap gap-2">
        {UTTERANCE_GAP_PRESETS_MS.map((ms) => (
          <button
            key={ms}
            type="button"
            onClick={() => setUtteranceGapMs(ms).then(() => setGapMs(ms))}
            className={`rounded border px-3 py-1 text-xs font-medium ${
              gapMs === ms
                ? "border-brand-400/70 bg-brand-500/10 text-brand-300"
                : "border-white/12 text-neutral-300 hover:bg-white/6"
            }`}
          >
            {ms} ms
          </button>
        ))}
      </div>
    </div>
  );
}

/**
 * Everything CLAUDE.md and docs/response-suggestion.md's diagnostic panel used to put
 * directly in the main window now lives only here, behind Settings → "Modo de
 * desenvolvedor". Same data as before (turn/utterance IDs, finalization reasons,
 * per-generation latency breakdown, raw provider prefix) — just relocated, not reduced.
 */
export function DeveloperToolsScreen({ onBack }: { onBack: () => void }) {
  const { turns, utterances } = useConversationTimeline();
  const { diagnostics } = useResponseSuggestions();
  const [copied, setCopied] = useState(false);

  const copyDiagnostics = async () => {
    const payload = { turns, utterances, diagnostics: Object.values(diagnostics) };
    await navigator.clipboard.writeText(JSON.stringify(payload, null, 2));
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1500);
  };

  return (
    <AppShell
      title="Diagnóstico"
      onBack={onBack}
      headerActions={
        <SecondaryButton onClick={copyDiagnostics} className="px-3 py-1.5 text-xs">
          {copied ? <Check className="h-3.5 w-3.5" /> : <Copy className="h-3.5 w-3.5" />}
          {copied ? "Copiado" : "Copiar diagnóstico"}
        </SecondaryButton>
      }
    >
      <div className="flex flex-col gap-6 pb-6 font-mono text-xs">
        <section className="flex flex-col gap-2">
          <h2 className="font-sans text-xs font-semibold uppercase tracking-wide text-neutral-500">
            Timer de utterance
          </h2>
          <UtteranceGapControl />
        </section>

        <section className="flex flex-col gap-2">
          <h2 className="font-sans text-xs font-semibold uppercase tracking-wide text-neutral-500">
            Turnos ({turns.length}) · Utterances ({utterances.length})
          </h2>
          <div className="flex flex-col gap-2">
            {turns.map((turn) => (
              <div key={turn.id} className="rounded border border-white/10 bg-surface p-2">
                <p className="text-neutral-300">
                  turn #{turn.id} · {turn.speaker} · {turn.source} · {turn.utterances.length} utterance(s) ·{" "}
                  {turn.finalized_at ? "finalizado" : "aberto"}
                </p>
                <p className="mt-1 text-neutral-500">{turn.text}</p>
                <div className="mt-1 flex flex-col gap-0.5 text-neutral-600">
                  {turn.utterances.map((id) => {
                    const utterance = utterances.find((u) => u.id === id);
                    return (
                      <p key={id}>
                        └─ utterance #{id} · rev {utterance?.revision ?? "?"} · segments:{" "}
                        {utterance?.segments.join(", ") ?? "?"}
                      </p>
                    );
                  })}
                </div>
              </div>
            ))}
            {turns.length === 0 && <p className="font-sans text-neutral-600">Nenhum turno ainda.</p>}
          </div>
        </section>

        <section className="flex flex-col gap-2">
          <h2 className="font-sans text-xs font-semibold uppercase tracking-wide text-neutral-500">
            Diagnóstico de geração
          </h2>
          <div className="flex flex-col gap-2">
            {Object.values(diagnostics)
              .sort((a, b) => a.turn_id - b.turn_id)
              .map((d) => (
                <div key={d.turn_id} className="rounded border border-white/10 bg-surface p-2 text-neutral-400">
                  <p className="text-neutral-300">
                    sessão #{d.session_id} · turn #{d.turn_id} · utterance #{d.utterance_id} · geração #
                    {d.generation_id} · {d.provider}/{d.model}
                  </p>
                  <p>
                    http_status: {d.http_status ?? "—"} · latency_ms: {d.latency_ms} · event_emitted:{" "}
                    <span className="text-brand-300">{d.event_emitted}</span>
                  </p>
                  <p>
                    finalization_reason: {d.finalization_reason || "—"} · gap_ms_used: {d.gap_ms_used} ·
                    silence_detected_ms: {d.silence_detected_ms ?? "—"}
                  </p>
                  <p>
                    utterance_finalized_to_request_started_ms: {d.utterance_finalized_to_request_started_ms ?? "—"} ·
                    request_to_first_http_chunk_ms: {d.request_to_first_http_chunk_ms ?? "—"}
                  </p>
                  <p className="text-emerald-400">
                    end_of_speech_to_first_visible_token_ms: {d.end_of_speech_to_first_visible_token_ms ?? "—"}
                  </p>
                  <p>
                    skip_detected: {String(d.skip_detected)} · echo_suppressed_characters:{" "}
                    {d.echo_suppressed_characters} · cancel_reason: {d.cancel_reason ?? "—"}
                  </p>
                  <p className="break-all">raw_prefix: {d.raw_prefix || "—"}</p>
                  <p>
                    context_turn_count: {d.context_turn_count} · context_character_count:{" "}
                    {d.context_character_count}
                  </p>
                  <p className="whitespace-pre-wrap break-all text-neutral-500">
                    prompt (sanitizado):{"\n"}
                    {d.prompt_preview || "—"}
                  </p>
                </div>
              ))}
            {Object.keys(diagnostics).length === 0 && (
              <p className="font-sans text-neutral-600">Nenhuma geração ainda.</p>
            )}
          </div>
        </section>
      </div>
    </AppShell>
  );
}
