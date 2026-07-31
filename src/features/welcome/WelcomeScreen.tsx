import { Rocket } from "lucide-react";
import { BrandMark } from "../../components/ui/BrandMark";

interface WelcomeScreenProps {
  onContinueWithoutLogin: () => void;
  onLogin: () => void;
}

export function WelcomeScreen({ onContinueWithoutLogin, onLogin }: WelcomeScreenProps) {
  return (
    <div className="flex h-full min-h-screen w-full items-center justify-center bg-black px-5 py-6">
      <div className="flex w-full max-w-[290px] items-center gap-2 rounded-[14px] border border-white/10 bg-[#101012] p-2 shadow-[0_14px_40px_rgba(0,0,0,.55)]">
        <button
          type="button"
          onClick={onLogin}
          className="grid h-10 w-10 place-items-center rounded-[10px] border border-white/8 bg-white/[0.04] text-white/76 hover:bg-white/8"
          aria-label="Entrar"
        >
          <BrandMark size={18} />
        </button>
        <button
          type="button"
          onClick={onContinueWithoutLogin}
          className="flex h-10 flex-1 items-center justify-center gap-2 rounded-[10px] bg-brand-600 px-3 text-xs font-bold text-white shadow-[0_0_20px_rgba(37,99,235,.42)] hover:bg-brand-500"
        >
          <Rocket className="h-4 w-4" />
          Iniciar Configuracao
        </button>
      </div>
    </div>
  );
}
