import { useEffect, useMemo, useRef, useState, type PointerEvent, type ReactNode } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  Bot,
  ChevronDown,
  Copy,
  FileText,
  Grip,
  Home,
  Loader2,
  Mic,
  Minus,
  MoreHorizontal,
  Pause,
  Play,
  RefreshCw,
  Settings,
  Terminal,
  Volume2,
  X,
} from "lucide-react";
import { BrandMark } from "../../components/ui/BrandMark";
import { IconButton } from "../../components/ui/IconButton";
import { Kbd } from "../../components/ui/Kbd";
import { StatusIndicator, type StatusTone } from "../../components/feedback/StatusIndicator";
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

interface SessionScreenProps {
  mode?: "combined" | "coordinator" | "ai" | "chat";
  startedAt: number;
  onOpenSettings: () => void;
  onOpenDeveloperTools: () => void;
  onEndSession: () => void;
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

function toneFor(status: CaptureStatus): StatusTone {
  if (status.kind === "capturing") return "active";
  if (status.kind === "error" || status.kind === "disconnected") return "error";
  if (status.kind === "switching") return "warning";
  return "neutral";
}

function useElapsed(startedAt: number) {
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    const interval = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(interval);
  }, []);

  return formatDuration(now - startedAt);
}

function startNativeDrag() {
  if (!("__TAURI_INTERNALS__" in window)) return;
  getCurrentWindow().startDragging().catch(() => {});
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
}: {
  exchange: Exchange | undefined;
  onRegenerate: (turnId: number) => void;
}) {
  const [copied, setCopied] = useState(false);
  const suggestion = exchange?.suggestion;
  const active = suggestion?.status === "preparing" || suggestion?.status === "streaming";
  const regenerateTurnId = exchange && exchange.turnId >= 0 ? exchange.turnId : null;

  const copy = async () => {
    if (!suggestion?.text) return;
    await navigator.clipboard.writeText(suggestion.text);
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1500);
  };

  let body: ReactNode = <p className="text-[15px] leading-relaxed text-white/62">Aguardando uma pergunta de OTHERS...</p>;

  if (suggestion?.status === "preparing") {
    body = (
      <div className="flex items-center gap-2 text-[15px] font-medium text-white/72">
        <Loader2 className="h-4 w-4 animate-spin text-blue-300" />
        Preparando resposta
      </div>
    );
  } else if (suggestion?.status === "error") {
    body = (
      <div className="flex flex-col gap-3">
        <p className="text-[15px] leading-relaxed text-red-200">
          {suggestion.errorMessage || "Nao foi possivel gerar a resposta."}
        </p>
        {regenerateTurnId !== null && (
          <button
            type="button"
            onClick={() => onRegenerate(regenerateTurnId)}
            className="inline-flex w-fit items-center gap-1.5 rounded-md bg-white/8 px-2.5 py-1.5 text-xs font-semibold text-white/80 hover:bg-white/12"
          >
            <RefreshCw className="h-3.5 w-3.5" />
            Tentar novamente
          </button>
        )}
      </div>
    );
  } else if (suggestion?.text) {
    body = (
      <p className="whitespace-pre-wrap text-[17px] leading-[1.42] text-white/86">
        {suggestion.text}
        {active && <span className="ml-1 inline-block h-4 w-1 translate-y-0.5 animate-pulse-soft bg-blue-300" />}
      </p>
    );
  } else if (suggestion?.status === "completed_empty" || suggestion?.status === "skipped") {
    body = <p className="text-[15px] leading-relaxed text-white/45">Sem sugestao para esta fala.</p>;
  }

  return (
    <>
      <div className="flex items-center justify-between px-3 pt-2">
        <div className="flex items-center gap-2 text-white/50">
          <BrandMark size={17} />
          <Grip className="h-3.5 w-3.5" />
        </div>
        <div className="flex items-center gap-1.5 text-white/56">
          {regenerateTurnId !== null && (
            <IconButton aria-label="Regenerar resposta" onClick={() => onRegenerate(regenerateTurnId)} className="h-7 w-7">
              <RefreshCw className="h-3.5 w-3.5" />
            </IconButton>
          )}
          <IconButton aria-label="Copiar resposta" onClick={copy} disabled={!suggestion?.text} className="h-7 w-7">
            <Copy className="h-3.5 w-3.5" />
          </IconButton>
          <IconButton aria-label="Minimizar resposta" className="h-7 w-7">
            <Minus className="h-3.5 w-3.5" />
          </IconButton>
        </div>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto px-4 pb-4 pt-3">{body}</div>
      <div className="flex items-center justify-between border-t border-white/8 px-3 py-2 text-[11px] text-white/38">
        <span>{exchange ? "1 / 1" : "0 / 0"}</span>
        <span>{copied ? "copiado" : active ? "streaming" : "Helppye"}</span>
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
        className={`max-w-[76%] rounded-[10px] border px-3.5 py-3 ${
          isUser
            ? "border-blue-400/12 bg-blue-950/24 text-right shadow-[inset_0_0_0_1px_rgba(37,99,255,.08)]"
            : "border-white/10 bg-[#171717] shadow-[0_1px_10px_rgba(0,0,0,.22)]"
        }`}
      >
        <p className={`mb-1.5 text-[11px] font-bold tracking-wide ${isUser ? "text-white/40" : "text-white/36"}`}>
          {label}
        </p>
        <p className="whitespace-pre-wrap text-[14px] leading-[1.45] text-white/74">{utterance.text || "..."}</p>
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
      className="flex w-full items-center gap-2 px-3 py-2 text-left text-xs font-medium text-white/70 hover:bg-white/8 hover:text-white"
    >
      {icon}
      {label}
    </button>
  );

  return (
    <div className="relative flex h-9 items-center gap-1 border-b border-white/10 bg-[#171717]/92 px-2">
      <button className="flex h-7 items-center gap-1.5 rounded-md bg-white/10 px-2.5 text-[11px] font-semibold text-white/70">
        <Mic className="h-3.5 w-3.5" />
        Transcription
      </button>
      <button className="flex h-7 items-center gap-1.5 rounded-md px-2 text-[11px] font-semibold text-white/48 hover:bg-white/6">
        <Bot className="h-3.5 w-3.5" />
        Session
      </button>
      <button className="flex h-7 items-center gap-1.5 rounded-md px-2 text-[11px] font-semibold text-white/48 hover:bg-white/6">
        <FileText className="h-3.5 w-3.5" />
        Summary
      </button>
      <div className="ml-auto flex items-center gap-1">
        <StatusIndicator label={listening ? "Ouvindo" : "Pausado"} tone={listening ? "active" : "neutral"} pulse={listening} />
        <IconButton aria-label="Configurar" onClick={onOpenSettings} className="h-7 w-7">
          <Settings className="h-3.5 w-3.5" />
        </IconButton>
        <IconButton aria-label="Mais opcoes" aria-expanded={menuOpen} onClick={() => setMenuOpen((v) => !v)} className="h-7 w-7">
          <MoreHorizontal className="h-3.5 w-3.5" />
        </IconButton>
      </div>
      {menuOpen && (
        <div className="absolute right-2 top-8 z-40 w-48 overflow-hidden rounded-lg border border-white/12 bg-[#19191b] py-1 shadow-raised">
          {menuItem(<FileText className="h-3.5 w-3.5" />, "Abrir transcricao", onOpenTranscript)}
          {devMode && menuItem(<Terminal className="h-3.5 w-3.5" />, "Diagnostico", onOpenDeveloperTools)}
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
    <div className="border-t border-white/10 bg-[#09090a] px-2 py-2">
      <div className="mb-2 flex items-center justify-between rounded-lg border border-white/8 bg-white/[0.04] px-3 py-2 text-xs font-semibold text-white/56">
        <span className="flex items-center gap-2">
          <Volume2 className="h-3.5 w-3.5" />
          Audio meters hidden
        </span>
        <span className="text-white/34">Click to show</span>
      </div>
      <div className="flex items-center gap-2">
        <div className="flex h-8 items-center gap-1 rounded-lg border border-white/8 bg-white/[0.04] px-2">
          <button type="button" className="grid h-5 w-5 place-items-center rounded text-white/70 hover:bg-white/8">
            <Pause className="h-3.5 w-3.5" />
          </button>
          <button type="button" className="grid h-5 w-5 place-items-center rounded text-white/70 hover:bg-white/8">
            <Play className="h-3.5 w-3.5" />
          </button>
        </div>
        <div className="flex h-8 min-w-0 flex-1 items-center justify-between rounded-lg border border-white/8 bg-white/[0.04] px-3 text-[11px] text-white/62">
          <span className="flex items-center gap-1.5 truncate">
            <span className="h-1.5 w-1.5 rounded-full bg-red-300" />
            Dutch
          </span>
          <ChevronDown className="h-3.5 w-3.5 shrink-0 text-white/38" />
        </div>
        <button
          type="button"
          className="flex h-8 items-center gap-2 rounded-lg bg-blue-600 px-3 text-[11px] font-bold leading-none text-white shadow-[0_0_18px_rgba(37,99,255,.45)] hover:bg-blue-500"
        >
          <Kbd keys={["mod", "D"]} />
          Analyze
        </button>
      </div>
      <div className="mt-2 flex items-center justify-between px-1 text-[10px] text-white/34">
        <div className="flex items-center gap-3">
          <StatusIndicator label="Mic" tone={toneFor(microphoneStatus)} pulse={microphoneStatus.kind === "capturing"} />
          <StatusIndicator label="System" tone={toneFor(systemStatus)} pulse={systemStatus.kind === "capturing"} />
        </div>
        <span className="font-mono">{elapsed}</span>
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
      <ol ref={scrollRef} className="min-h-0 flex-1 space-y-3 overflow-y-auto bg-[#050607] px-3 py-3">
        {utterances.length === 0 ? (
          <li className="rounded-lg border border-white/8 bg-white/[0.03] px-3 py-3 text-sm text-white/42">
            Aguardando a linha do tempo da conversa...
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
}: SessionScreenProps) {
  const [transcriptOpen, setTranscriptOpen] = useState(false);
  const [aiPosition, setAiPosition] = useState<PanelPosition>({ x: 14, y: 10 });
  const [chatPosition, setChatPosition] = useState<PanelPosition>({ x: 14, y: 208 });
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
      <div className="flex h-full min-h-screen w-full items-center justify-center bg-[#050506] px-2">
        <div className="flex h-[58px] w-full items-center gap-2 rounded-[14px] border border-white/10 bg-[#101012] p-2 shadow-[0_14px_40px_rgba(0,0,0,.55)]">
          <div className="grid h-10 w-10 place-items-center rounded-[10px] border border-white/8 bg-black/30 text-white/42">
            <Grip className="h-4 w-4" />
          </div>
          <button
            type="button"
            onClick={onEndSession}
            className="grid h-10 w-10 place-items-center rounded-[10px] border border-white/8 bg-white/[0.04] text-white/76 hover:bg-white/8"
            aria-label="Encerrar sessao"
          >
            <Home className="h-4 w-4" />
          </button>
          <button
            type="button"
            onClick={onOpenSettings}
            className="flex h-10 flex-1 items-center justify-center gap-2 rounded-[10px] bg-blue-600 px-3 text-xs font-bold text-white shadow-[0_0_20px_rgba(37,99,255,.42)] hover:bg-blue-500"
          >
            <Settings className="h-4 w-4" />
            Sessao ativa
          </button>
        </div>
      </div>
    );
  }

  if (mode === "ai") {
    return (
      <div
        className="flex h-full min-h-screen w-full flex-col overflow-hidden rounded-[12px] border border-white/10 bg-black/48 shadow-[0_18px_55px_rgba(0,0,0,.55)] backdrop-blur-xl"
        onPointerDown={(event) => {
          if ((event.target as HTMLElement).closest("button")) return;
          startNativeDrag();
        }}
      >
        <AiResponsePanel exchange={activeExchange} onRegenerate={handleRegenerate} />
      </div>
    );
  }

  if (mode === "chat") {
    return (
      <div className="flex h-full min-h-screen w-full flex-col overflow-hidden rounded-[12px] border border-white/10 bg-[#050607] shadow-[0_18px_55px_rgba(0,0,0,.5)]">
        <div
          className="flex h-7 items-center justify-between border-b border-white/8 bg-[#101012] px-2"
          onPointerDown={startNativeDrag}
        >
          <div className="flex items-center gap-2 text-white/46">
            <Grip className="h-3.5 w-3.5" />
            <Home className="h-3.5 w-3.5" />
            <span className="text-[11px] font-semibold">Helppye Session</span>
          </div>
          <Minus className="h-3.5 w-3.5 text-white/38" />
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
    <div className="relative h-full min-h-screen w-full overflow-hidden bg-[#020207]">
      <div className="absolute inset-0 bg-[radial-gradient(circle_at_30%_4%,rgba(37,99,255,.22),transparent_34%),linear-gradient(180deg,rgba(14,14,22,.95),#020207_58%)]" />
      <DraggablePanel
        position={aiPosition}
        onPositionChange={setAiPosition}
        className="absolute left-0 top-0 z-20 flex h-[184px] w-[calc(100%-28px)] min-w-[320px] flex-col overflow-hidden rounded-[12px] border border-white/10 bg-black/48 shadow-[0_18px_55px_rgba(0,0,0,.55)] backdrop-blur-xl"
        handle={<AiResponsePanel exchange={activeExchange} onRegenerate={handleRegenerate} />}
      >
        {null}
      </DraggablePanel>
      <DraggablePanel
        position={chatPosition}
        onPositionChange={setChatPosition}
        className="absolute left-0 top-0 z-10 flex h-[calc(100%-220px)] min-h-[340px] w-[calc(100%-28px)] min-w-[320px] flex-col overflow-hidden rounded-[12px] border border-white/10 bg-[#050607] shadow-[0_18px_55px_rgba(0,0,0,.5)]"
        handle={
          <div className="flex h-7 items-center justify-between border-b border-white/8 bg-[#101012] px-2">
            <div className="flex items-center gap-2 text-white/46">
              <Grip className="h-3.5 w-3.5" />
              <Home className="h-3.5 w-3.5" />
              <span className="text-[11px] font-semibold">Helppye Session</span>
            </div>
            <Minus className="h-3.5 w-3.5 text-white/38" />
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
