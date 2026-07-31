import { useEffect, useMemo, useRef, useState, type PointerEvent, type ReactNode } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Copy, FileText, Grip, Loader2, Minus, MoreHorizontal, RefreshCw, Settings, Terminal, X } from "lucide-react";
import { IconButton } from "../../components/ui/IconButton";
import { TranscriptDrawer } from "./TranscriptDrawer";
import { useConversationTimeline } from "../../hooks/useConversationTimeline";
import { useResponseSuggestions } from "../../hooks/useResponseSuggestions";
import { useAudioCapture } from "../../hooks/useAudioCapture";
import { useOnboardingStore } from "../../stores/useOnboardingStore";
import { regenerateSuggestion } from "../../services/conversationService";
import { useKeyboardShortcuts } from "../../hooks/useKeyboardShortcuts";
import { formatDuration } from "../../utils/format";
import type { CaptureStatus } from "../../stores/useAudioCaptureStore";
import type { ConversationUtterance } from "../../types/conversation";
import type { SuggestionState } from "./responseSuggestionViewModel";

type SessionMode = "combined" | "coordinator" | "ai" | "chat";

interface SessionScreenProps {
  mode?: SessionMode;
  startedAt: number;
  onOpenSettings: () => void;
  onOpenDeveloperTools: () => void;
  onEndSession: () => void;
  onRestoreSession?: () => void;
}

interface Exchange {
  utteranceId: number;
  turnId: number;
  question: string;
  suggestion: SuggestionState | undefined;
}

interface PanelPosition {
  x: number;
  y: number;
}

function isEligible(item: { speaker: string; source: string }): boolean {
  return item.speaker === "other_person" && item.source === "system_output";
}

function isTauriRuntime(): boolean {
  return "__TAURI_INTERNALS__" in window;
}

function startNativeDrag() {
  if (!isTauriRuntime()) return;
  getCurrentWindow().startDragging().catch(() => {});
}

function minimizeWindow() {
  if (!isTauriRuntime()) return;
  getCurrentWindow().minimize().catch(() => {});
}

function useElapsed(startedAt: number) {
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    const interval = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(interval);
  }, []);

  return formatDuration(now - startedAt);
}

function CaptureDot({ status, label }: { status: CaptureStatus; label: string }) {
  const active = status.kind === "capturing";
  const error = status.kind === "error" || status.kind === "disconnected";

  return (
    <span className="inline-flex items-center gap-1.5 text-[11px] font-medium text-white/45">
      <span className={`h-1.5 w-1.5 rounded-full ${error ? "bg-red-300" : active ? "bg-emerald-300" : "bg-white/24"}`} />
      {label}
    </span>
  );
}

function DraggablePanel({
  position,
  onPositionChange,
  className,
  handle,
  children,
}: {
  position: PanelPosition;
  onPositionChange: (position: PanelPosition) => void;
  className: string;
  handle: ReactNode;
  children: ReactNode;
}) {
  const dragRef = useRef<{ pointerId: number; dx: number; dy: number } | null>(null);

  const startDrag = (event: PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    dragRef.current = {
      pointerId: event.pointerId,
      dx: event.clientX - position.x,
      dy: event.clientY - position.y,
    };
    event.currentTarget.setPointerCapture(event.pointerId);
  };

  const moveDrag = (event: PointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    if (!drag || drag.pointerId !== event.pointerId) return;
    onPositionChange({
      x: Math.max(8, event.clientX - drag.dx),
      y: Math.max(8, event.clientY - drag.dy),
    });
  };

  const endDrag = (event: PointerEvent<HTMLDivElement>) => {
    if (dragRef.current?.pointerId !== event.pointerId) return;
    dragRef.current = null;
    event.currentTarget.releasePointerCapture(event.pointerId);
  };

  return (
    <section className={className} style={{ transform: `translate3d(${position.x}px, ${position.y}px, 0)` }}>
      <div
        role="presentation"
        className="cursor-grab select-none active:cursor-grabbing"
        onPointerDown={startDrag}
        onPointerMove={moveDrag}
        onPointerUp={endDrag}
        onPointerCancel={endDrag}
      >
        {handle}
      </div>
      {children}
    </section>
  );
}

function AiResponsePanel({
  exchange,
  onRegenerate,
  nativeDrag,
}: {
  exchange: Exchange | undefined;
  onRegenerate: (turnId: number) => void;
  nativeDrag?: boolean;
}) {
  const [copied, setCopied] = useState(false);
  const suggestion = exchange?.suggestion;
  const active = suggestion?.status === "preparing" || suggestion?.status === "streaming";
  const regenerateTurnId = exchange && exchange.turnId >= 0 ? exchange.turnId : null;

  const copy = async () => {
    if (!suggestion?.text) return;
    await navigator.clipboard.writeText(suggestion.text);
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1400);
  };

  let body: ReactNode = <p className="text-[15px] leading-relaxed text-white/50">Aguardando uma fala de OTHERS...</p>;

  if (suggestion?.status === "preparing") {
    body = (
      <div className="flex items-center gap-2 text-[15px] font-medium text-white/62">
        <Loader2 className="h-4 w-4 animate-spin text-white/50" />
        Preparando resposta
      </div>
    );
  } else if (suggestion?.status === "error") {
    body = (
      <div className="flex flex-col gap-3">
        <p className="text-[15px] leading-relaxed text-red-200/90">
          {suggestion.errorMessage || "Nao foi possivel gerar a resposta."}
        </p>
        {regenerateTurnId !== null && (
          <button
            type="button"
            onClick={() => onRegenerate(regenerateTurnId)}
            className="inline-flex w-fit items-center gap-1.5 rounded-full bg-white/8 px-3 py-1.5 text-xs font-medium text-white/72 transition hover:bg-white/12"
          >
            <RefreshCw className="h-3.5 w-3.5" />
            Tentar novamente
          </button>
        )}
      </div>
    );
  } else if (suggestion?.text) {
    body = (
      <p className="whitespace-pre-wrap text-[18px] font-medium leading-[1.42] tracking-normal text-white/84">
        {suggestion.text}
        {active && <span className="ml-1 inline-block h-4 w-1 translate-y-0.5 animate-pulse-soft rounded-full bg-white/55" />}
      </p>
    );
  } else if (suggestion?.status === "completed_empty" || suggestion?.status === "skipped") {
    body = <p className="text-[15px] leading-relaxed text-white/42">Sem sugestao para esta fala.</p>;
  }

  return (
    <>
      <div
        className="flex h-10 items-center justify-between px-4"
        onPointerDown={(event) => {
          if (!nativeDrag || (event.target as HTMLElement).closest("button")) return;
          startNativeDrag();
        }}
      >
        <div className="flex items-center gap-2 text-white/38">
          <Grip className="h-3.5 w-3.5" />
          <span className="text-[11px] font-medium">Helppye</span>
        </div>
        <div className="flex items-center gap-1">
          {regenerateTurnId !== null && (
            <IconButton aria-label="Regenerar resposta" onClick={() => onRegenerate(regenerateTurnId)} className="h-7 w-7 rounded-full">
              <RefreshCw className="h-3.5 w-3.5" />
            </IconButton>
          )}
          <IconButton aria-label="Copiar resposta" onClick={copy} disabled={!suggestion?.text} className="h-7 w-7 rounded-full">
            <Copy className="h-3.5 w-3.5" />
          </IconButton>
          <IconButton aria-label="Minimizar resposta" onClick={minimizeWindow} className="h-7 w-7 rounded-full">
            <Minus className="h-3.5 w-3.5" />
          </IconButton>
        </div>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto px-5 pb-4 pt-1">{body}</div>
      <div className="flex h-8 items-center justify-between px-4 text-[11px] text-white/34">
        <span>{exchange ? "1 / 1" : "0 / 0"}</span>
        <span>{copied ? "copiado" : active ? "gerando" : "resposta"}</span>
      </div>
    </>
  );
}

function TimelineMessage({ utterance }: { utterance: ConversationUtterance }) {
  const isUser = utterance.speaker === "user";
  const label = isUser ? "YOU" : "OTHERS";

  return (
    <li className={`flex ${isUser ? "justify-end" : "justify-start"}`}>
      <div
        className={`max-w-[78%] rounded-[18px] px-4 py-3 ${
          isUser
            ? "bg-white/[0.08] text-right shadow-[inset_0_0_0_1px_rgba(255,255,255,.04)]"
            : "bg-white/[0.055] shadow-[inset_0_0_0_1px_rgba(255,255,255,.055)]"
        }`}
      >
        <p className="mb-1.5 text-[10px] font-semibold tracking-wide text-white/32">{label}</p>
        <p className="whitespace-pre-wrap text-[14px] leading-[1.48] text-white/76">{utterance.text || "..."}</p>
      </div>
    </li>
  );
}

function ChatToolbar({
  listening,
  devMode,
  onOpenSettings,
  onOpenTranscript,
  onOpenDeveloperTools,
  onEndSession,
}: {
  listening: boolean;
  devMode: boolean;
  onOpenSettings: () => void;
  onOpenTranscript: () => void;
  onOpenDeveloperTools: () => void;
  onEndSession: () => void;
}) {
  const [menuOpen, setMenuOpen] = useState(false);

  const menuItem = (icon: ReactNode, label: string, onClick: () => void) => (
    <button
      type="button"
      onClick={() => {
        setMenuOpen(false);
        onClick();
      }}
      className="flex w-full items-center gap-2 rounded-[10px] px-3 py-2 text-left text-xs font-medium text-white/70 transition hover:bg-white/8 hover:text-white"
    >
      {icon}
      {label}
    </button>
  );

  return (
    <div className="relative flex h-12 items-center gap-2 border-b border-white/[0.06] bg-white/[0.035] px-3 backdrop-blur-xl">
      <button
        type="button"
        onClick={onOpenTranscript}
        className="inline-flex h-8 items-center gap-2 rounded-full bg-white/[0.07] px-3 text-[11px] font-medium text-white/70 transition hover:bg-white/[0.11]"
      >
        <FileText className="h-3.5 w-3.5" />
        Transcricao
      </button>
      <span className="inline-flex h-8 items-center gap-2 rounded-full px-2.5 text-[11px] font-medium text-white/44">
        <span className={`h-1.5 w-1.5 rounded-full ${listening ? "bg-emerald-300" : "bg-white/24"}`} />
        {listening ? "Ouvindo" : "Pausado"}
      </span>
      <div className="ml-auto flex items-center gap-1">
        <IconButton aria-label="Configurar" onClick={onOpenSettings} className="h-8 w-8 rounded-full">
          <Settings className="h-3.5 w-3.5" />
        </IconButton>
        <IconButton aria-label="Mais opcoes" aria-expanded={menuOpen} onClick={() => setMenuOpen((v) => !v)} className="h-8 w-8 rounded-full">
          <MoreHorizontal className="h-3.5 w-3.5" />
        </IconButton>
      </div>
      {menuOpen && (
        <div className="absolute right-3 top-11 z-40 w-52 overflow-hidden rounded-[16px] border border-white/10 bg-[#1c1c1e]/95 p-1.5 shadow-[0_18px_55px_rgba(0,0,0,.45)] backdrop-blur-xl">
          {menuItem(<FileText className="h-3.5 w-3.5" />, "Abrir transcricao", onOpenTranscript)}
          {devMode && menuItem(<Terminal className="h-3.5 w-3.5" />, "Diagnostico", onOpenDeveloperTools)}
          {menuItem(<Minus className="h-3.5 w-3.5" />, "Minimizar", minimizeWindow)}
          <div className="my-1 border-t border-white/8" />
          {menuItem(<X className="h-3.5 w-3.5 text-red-300" />, "Encerrar sessao", onEndSession)}
        </div>
      )}
    </div>
  );
}

function ChatFooter({
  microphoneStatus,
  systemStatus,
  startedAt,
}: {
  microphoneStatus: CaptureStatus;
  systemStatus: CaptureStatus;
  startedAt: number;
}) {
  const elapsed = useElapsed(startedAt);

  return (
    <div className="border-t border-white/[0.06] bg-white/[0.025] px-4 py-3 backdrop-blur-xl">
      <div className="flex items-center justify-between gap-3">
        <div className="flex min-w-0 items-center gap-3">
          <CaptureDot status={microphoneStatus} label="Mic" />
          <CaptureDot status={systemStatus} label="Sistema" />
        </div>
        <div className="flex items-center gap-2 text-[11px] font-medium text-white/42">
          <span>Portugues</span>
          <span className="font-mono text-white/32">{elapsed}</span>
        </div>
      </div>
    </div>
  );
}

function ChatTimelinePanel({
  utterances,
  listening,
  devMode,
  microphoneStatus,
  systemStatus,
  startedAt,
  onOpenSettings,
  onOpenTranscript,
  onOpenDeveloperTools,
  onEndSession,
}: {
  utterances: ConversationUtterance[];
  listening: boolean;
  devMode: boolean;
  microphoneStatus: CaptureStatus;
  systemStatus: CaptureStatus;
  startedAt: number;
  onOpenSettings: () => void;
  onOpenTranscript: () => void;
  onOpenDeveloperTools: () => void;
  onEndSession: () => void;
}) {
  const scrollRef = useRef<HTMLOListElement>(null);
  const lastKey = utterances.map((u) => `${u.id}:${u.text.length}`).join("|");

  useEffect(() => {
    const node = scrollRef.current;
    if (node) node.scrollTop = node.scrollHeight;
  }, [lastKey]);

  return (
    <>
      <ChatToolbar
        listening={listening}
        devMode={devMode}
        onOpenSettings={onOpenSettings}
        onOpenTranscript={onOpenTranscript}
        onOpenDeveloperTools={onOpenDeveloperTools}
        onEndSession={onEndSession}
      />
      <ol ref={scrollRef} className="min-h-0 flex-1 space-y-3 overflow-y-auto px-3 py-4">
        {utterances.length === 0 ? (
          <li className="rounded-[18px] bg-white/[0.055] px-4 py-3 text-sm text-white/42">
            Aguardando a conversa...
          </li>
        ) : (
          utterances.map((utterance) => <TimelineMessage key={utterance.id} utterance={utterance} />)
        )}
      </ol>
      <ChatFooter microphoneStatus={microphoneStatus} systemStatus={systemStatus} startedAt={startedAt} />
    </>
  );
}

export function SessionScreen({
  mode = "combined",
  startedAt,
  onOpenSettings,
  onOpenDeveloperTools,
  onEndSession,
  onRestoreSession,
}: SessionScreenProps) {
  const [transcriptOpen, setTranscriptOpen] = useState(false);
  const [aiPosition, setAiPosition] = useState<PanelPosition>({ x: 12, y: 10 });
  const [chatPosition, setChatPosition] = useState<PanelPosition>({ x: 12, y: 210 });
  const { turns, utterances } = useConversationTimeline();
  const { suggestions } = useResponseSuggestions();
  const microphone = useAudioCapture("microphone");
  const systemOutput = useAudioCapture("system_output");
  const devMode = useOnboardingStore((s) => s.devMode);

  const exchanges = useMemo<Exchange[]>(() => {
    const turnOf = new Map<number, number>();
    for (const turn of turns) {
      for (const utteranceId of turn.utterances) turnOf.set(utteranceId, turn.id);
    }

    return utterances
      .filter((utterance) => isEligible(utterance) && utterance.text.trim().length > 0)
      .map((utterance) => ({
        utteranceId: utterance.id,
        turnId: turnOf.get(utterance.id) ?? suggestions[utterance.id]?.turnId ?? -1,
        question: utterance.text,
        suggestion: suggestions[utterance.id],
      }));
  }, [turns, utterances, suggestions]);

  const activeExchange =
    [...exchanges].reverse().find((exchange) => exchange.suggestion?.status === "streaming" || exchange.suggestion?.status === "preparing") ??
    [...exchanges].reverse().find((exchange) => exchange.suggestion?.text) ??
    exchanges[exchanges.length - 1];

  const handleRegenerate = (turnId: number) => {
    if (turnId >= 0) void regenerateSuggestion(turnId);
  };

  const latestTurnId = activeExchange?.turnId ?? -1;
  const listening = microphone.status.kind === "capturing" || systemOutput.status.kind === "capturing";

  useKeyboardShortcuts({
    onToggleSession: onEndSession,
    onOpenSettings,
    onRegenerate: () => handleRegenerate(latestTurnId),
  });

  if (mode === "coordinator") {
    return (
      <div className="flex h-full min-h-screen w-full items-center justify-center bg-transparent px-2">
        <div className="flex h-[58px] w-full items-center gap-2 rounded-[18px] border border-white/10 bg-[#1c1c1e]/86 p-2 shadow-[0_18px_45px_rgba(0,0,0,.38)] backdrop-blur-xl">
          <div className="grid h-10 w-10 place-items-center rounded-full bg-white/[0.06] text-white/36">
            <Grip className="h-4 w-4" />
          </div>
          <button
            type="button"
            onClick={onOpenSettings}
            className="grid h-10 w-10 place-items-center rounded-full bg-white/[0.06] text-white/70 transition hover:bg-white/[0.1]"
            aria-label="Configuracoes"
          >
            <Settings className="h-4 w-4" />
          </button>
          <button
            type="button"
            onClick={onRestoreSession}
            className="flex h-10 flex-1 items-center justify-center rounded-full bg-white text-xs font-semibold text-black transition hover:bg-white/88"
          >
            Mostrar sessao
          </button>
          <button
            type="button"
            onClick={onEndSession}
            className="grid h-7 w-7 place-items-center rounded-full text-white/34 transition hover:bg-white/[0.08] hover:text-white/72"
            aria-label="Encerrar sessao"
          >
            <X className="h-3.5 w-3.5" />
          </button>
        </div>
      </div>
    );
  }

  if (mode === "ai") {
    return (
      <div className="flex h-full min-h-screen w-full flex-col overflow-hidden rounded-[22px] border border-white/10 bg-[#111113]/62 shadow-[0_22px_70px_rgba(0,0,0,.45)] backdrop-blur-2xl">
        <AiResponsePanel exchange={activeExchange} onRegenerate={handleRegenerate} nativeDrag />
      </div>
    );
  }

  if (mode === "chat") {
    return (
      <div className="flex h-full min-h-screen w-full flex-col overflow-hidden rounded-[22px] border border-white/[0.08] bg-[#101012]/96 shadow-[0_22px_70px_rgba(0,0,0,.45)]">
        <div
          className="flex h-8 items-center justify-between px-4 text-white/36"
          onPointerDown={(event) => {
            if ((event.target as HTMLElement).closest("button")) return;
            startNativeDrag();
          }}
        >
          <div className="flex items-center gap-2">
            <Grip className="h-3.5 w-3.5" />
            <span className="text-[11px] font-medium">Helppye Session</span>
          </div>
          <button type="button" onClick={minimizeWindow} aria-label="Minimizar chat" className="rounded-full p-1 transition hover:bg-white/8">
            <Minus className="h-3.5 w-3.5" />
          </button>
        </div>
        <ChatTimelinePanel
          utterances={utterances}
          listening={listening}
          devMode={devMode}
          microphoneStatus={microphone.status}
          systemStatus={systemOutput.status}
          startedAt={startedAt}
          onOpenSettings={onOpenSettings}
          onOpenTranscript={() => setTranscriptOpen(true)}
          onOpenDeveloperTools={onOpenDeveloperTools}
          onEndSession={onEndSession}
        />
        <TranscriptDrawer open={transcriptOpen} onClose={() => setTranscriptOpen(false)} utterances={utterances} />
      </div>
    );
  }

  return (
    <div className="relative h-full min-h-screen w-full overflow-hidden bg-[#09090b]">
      <div className="absolute inset-0 bg-[radial-gradient(circle_at_50%_0%,rgba(255,255,255,.08),transparent_38%)]" />
      <DraggablePanel
        position={aiPosition}
        onPositionChange={setAiPosition}
        className="absolute left-0 top-0 z-20 flex h-[190px] w-[calc(100%-24px)] min-w-[320px] flex-col overflow-hidden rounded-[22px] border border-white/10 bg-[#111113]/72 shadow-[0_22px_70px_rgba(0,0,0,.45)] backdrop-blur-2xl"
        handle={<AiResponsePanel exchange={activeExchange} onRegenerate={handleRegenerate} />}
      >
        {null}
      </DraggablePanel>
      <DraggablePanel
        position={chatPosition}
        onPositionChange={setChatPosition}
        className="absolute left-0 top-0 z-10 flex h-[calc(100%-222px)] min-h-[340px] w-[calc(100%-24px)] min-w-[320px] flex-col overflow-hidden rounded-[22px] border border-white/[0.08] bg-[#101012]/96 shadow-[0_22px_70px_rgba(0,0,0,.45)]"
        handle={
          <div className="flex h-8 items-center justify-between px-4 text-white/36">
            <div className="flex items-center gap-2">
              <Grip className="h-3.5 w-3.5" />
              <span className="text-[11px] font-medium">Helppye Session</span>
            </div>
            <Minus className="h-3.5 w-3.5" />
          </div>
        }
      >
        <ChatTimelinePanel
          utterances={utterances}
          listening={listening}
          devMode={devMode}
          microphoneStatus={microphone.status}
          systemStatus={systemOutput.status}
          startedAt={startedAt}
          onOpenSettings={onOpenSettings}
          onOpenTranscript={() => setTranscriptOpen(true)}
          onOpenDeveloperTools={onOpenDeveloperTools}
          onEndSession={onEndSession}
        />
      </DraggablePanel>
      <TranscriptDrawer open={transcriptOpen} onClose={() => setTranscriptOpen(false)} utterances={utterances} />
    </div>
  );
}
