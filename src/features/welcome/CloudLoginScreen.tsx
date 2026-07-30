import { Cloud } from "lucide-react";
import { PrimaryButton } from "../../components/ui/PrimaryButton";
import { GhostButton } from "../../components/ui/GhostButton";

interface CloudLoginScreenProps {
  onContinueWithoutLogin: () => void;
  onBack: () => void;
}

/** An honest dead end, not a fake login form. There is no backend for this yet — showing
 * an email/password form that goes nowhere would be worse than admitting it plainly. */
export function CloudLoginScreen({ onContinueWithoutLogin, onBack }: CloudLoginScreenProps) {
  return (
    <div className="flex h-full min-h-screen w-full flex-col items-center justify-center gap-6 bg-app px-8 py-10 text-center">
      <span className="flex h-12 w-12 items-center justify-center rounded-full bg-white/6 text-neutral-400">
        <Cloud className="h-5 w-5" />
      </span>

      <div className="flex max-w-xs flex-col gap-2">
        <h1 className="text-lg font-semibold text-neutral-50">Helppye Cloud está chegando</h1>
        <p className="text-sm leading-relaxed text-neutral-400">
          Nesta versão, você já pode usar todos os recursos locais sem criar uma conta.
        </p>
      </div>

      <div className="flex w-full max-w-xs flex-col items-center gap-2">
        <PrimaryButton fullWidth onClick={onContinueWithoutLogin}>
          Continuar sem login
        </PrimaryButton>
        <GhostButton onClick={onBack}>Voltar</GhostButton>
      </div>
    </div>
  );
}
