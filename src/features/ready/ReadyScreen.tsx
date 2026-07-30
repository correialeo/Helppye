import { Settings } from "lucide-react";
import { BrandMark } from "../../components/ui/BrandMark";
import { PrimaryButton } from "../../components/ui/PrimaryButton";
import { IconButton } from "../../components/ui/IconButton";
import { Kbd } from "../../components/ui/Kbd";
import { useOnboardingStore } from "../../stores/useOnboardingStore";
import { useResponseProvider } from "../../hooks/useResponseProvider";
import { useKeyboardShortcuts } from "../../hooks/useKeyboardShortcuts";

interface ReadyScreenProps {
  onStartSession: () => void;
  onOpenSettings: () => void;
}

const PROVIDER_LABEL: Record<string, string> = {
  ollama: "Ollama",
  open_ai: "OpenAI",
  anthropic: "Anthropic",
  deep_seek: "DeepSeek",
};

/** The landing screen for every return visit — high-impact like Welcome, but with a
 * single secondary line acknowledging the setup exists instead of hiding it entirely.
 * "Começar sessão" is the only dominant action; settings live behind a quiet corner icon
 * and Ctrl/Cmd+Enter, never competing with it for attention. */
export function ReadyScreen({ onStartSession, onOpenSettings }: ReadyScreenProps) {
  const userName = useOnboardingStore((s) => s.userName);
  const { status } = useResponseProvider();
  useKeyboardShortcuts({ onToggleSession: onStartSession, onOpenSettings });

  return (
    <div className="relative flex h-full min-h-screen w-full flex-col items-center justify-center gap-8 bg-app px-8 py-10 text-center">
      <IconButton aria-label="Configurações" onClick={onOpenSettings} className="absolute right-4 top-4">
        <Settings className="h-4 w-4" />
      </IconButton>

      <BrandMark size={44} />

      <div className="flex max-w-xs flex-col gap-2">
        <h1 className="text-xl font-semibold tracking-tight text-neutral-50">
          {userName ? `Tudo pronto, ${userName}` : "Tudo pronto"}
        </h1>
        <p className="text-sm leading-relaxed text-neutral-400">
          O Helppye está preparado para acompanhar sua próxima conversa.
        </p>
      </div>

      <div className="flex flex-col items-center gap-2">
        <PrimaryButton onClick={onStartSession}>Começar sessão</PrimaryButton>
        <p className="flex items-center gap-1.5 text-xs text-neutral-600">
          <Kbd keys={["mod", "D"]} /> para iniciar rapidamente
        </p>
      </div>

      {status && (
        <p className="absolute bottom-6 flex items-center gap-2 text-xs text-neutral-600">
          <span>
            {PROVIDER_LABEL[status.provider]} · {status.model}
          </span>
          <span aria-hidden="true">·</span>
          <span>Português</span>
          <span aria-hidden="true">·</span>
          <span>Áudio pronto</span>
        </p>
      )}
    </div>
  );
}
