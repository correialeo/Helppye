import { Grip, Home, Rocket, Settings } from "lucide-react";

interface ReadyScreenProps {
  onStartSession: () => void;
  onOpenSettings: () => void;
}

export function ReadyScreen({ onStartSession, onOpenSettings }: ReadyScreenProps) {
  return (
    <div className="flex h-full min-h-screen w-full items-center justify-center bg-black px-5 py-6">
      <div className="flex w-full max-w-[290px] items-center gap-2 rounded-[14px] border border-white/10 bg-[#101012] p-2 shadow-[0_14px_40px_rgba(0,0,0,.55)]">
        <div className="grid h-10 w-10 place-items-center rounded-[10px] border border-white/8 bg-black/30 text-white/42">
          <Grip className="h-4 w-4" />
        </div>
        <button
          type="button"
          onClick={onOpenSettings}
          className="grid h-10 w-10 place-items-center rounded-[10px] border border-white/8 bg-white/[0.04] text-white/76 hover:bg-white/8"
          aria-label="Configuracoes"
        >
          <Settings className="h-4 w-4" />
        </button>
        <button
          type="button"
          onClick={onStartSession}
          className="flex h-10 flex-1 items-center justify-center gap-2 rounded-[10px] bg-brand-600 px-3 text-xs font-bold text-white shadow-[0_0_20px_rgba(37,99,235,.42)] hover:bg-brand-500"
        >
          <Rocket className="h-4 w-4" />
          Iniciar Sessao
        </button>
        <Home className="hidden" aria-hidden="true" />
      </div>
    </div>
  );
}
