import { getCurrentWindow } from "@tauri-apps/api/window";
import { Minus, Rocket, X } from "lucide-react";
import { BrandMark } from "../../components/ui/BrandMark";
import { useTransparentWindowBackground } from "../../hooks/useTransparentWindowBackground";

interface WelcomeScreenProps {
  onContinueWithoutLogin: () => void;
  onLogin: () => void;
}

function isTauriRuntime(): boolean {
  return "__TAURI_INTERNALS__" in window;
}

function closeWindow() {
  if (!isTauriRuntime()) return;
  getCurrentWindow().close().catch(() => {});
}

function minimizeWindow() {
  if (!isTauriRuntime()) return;
  getCurrentWindow().minimize().catch(() => {});
}

function startDrag() {
  if (!isTauriRuntime()) return;
  getCurrentWindow().startDragging().catch(() => {});
}

export function WelcomeScreen({ onContinueWithoutLogin, onLogin }: WelcomeScreenProps) {
  useTransparentWindowBackground();

  return (
    <div className="flex h-full min-h-screen w-full items-center justify-center bg-transparent px-2 py-2">
      <div
        className="flex h-[58px] w-full items-center gap-2 rounded-[22px] bg-[#050506] p-2"
        onPointerDown={(event) => {
          if ((event.target as HTMLElement).closest("button")) return;
          startDrag();
        }}
      >
        <button
          type="button"
          onClick={onLogin}
          className="grid h-10 w-10 place-items-center rounded-full bg-white/[0.07] text-white/72 transition hover:bg-white/[0.12]"
          aria-label="Entrar"
        >
          <BrandMark size={18} />
        </button>
        <button
          type="button"
          onClick={onContinueWithoutLogin}
          className="flex h-10 flex-1 items-center justify-center gap-2 rounded-full bg-white px-3 text-xs font-semibold text-black transition hover:bg-white/88"
        >
          <Rocket className="h-4 w-4" />
          Configurar
        </button>
        <button
          type="button"
          onClick={minimizeWindow}
          className="grid h-7 w-7 place-items-center rounded-full text-white/34 transition hover:bg-white/[0.08] hover:text-white/72"
          aria-label="Minimizar Helppye"
        >
          <Minus className="h-3.5 w-3.5" />
        </button>
        <button
          type="button"
          onClick={closeWindow}
          className="grid h-7 w-7 place-items-center rounded-full text-white/34 transition hover:bg-white/[0.08] hover:text-white/72"
          aria-label="Fechar Helppye"
        >
          <X className="h-3.5 w-3.5" />
        </button>
      </div>
    </div>
  );
}
