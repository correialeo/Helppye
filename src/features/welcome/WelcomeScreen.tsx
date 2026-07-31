import { getCurrentWindow } from "@tauri-apps/api/window";
import { Rocket, X } from "lucide-react";
import { BrandMark } from "../../components/ui/BrandMark";

interface WelcomeScreenProps {
  onContinueWithoutLogin: () => void;
  onLogin: () => void;
}

function closeWindow() {
  if (!("__TAURI_INTERNALS__" in window)) return;
  getCurrentWindow().close().catch(() => {});
}

export function WelcomeScreen({ onContinueWithoutLogin, onLogin }: WelcomeScreenProps) {
  return (
    <div className="flex h-full min-h-screen w-full items-center justify-center bg-transparent px-2 py-2">
      <div className="flex h-[58px] w-full items-center gap-2 rounded-[22px] bg-[#111113]/94 p-2 shadow-[0_18px_55px_rgba(0,0,0,.46)] backdrop-blur-2xl">
        <button
          type="button"
          onClick={onLogin}
          className="grid h-10 w-10 place-items-center rounded-full bg-white/[0.06] text-white/72 transition hover:bg-white/[0.1]"
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
