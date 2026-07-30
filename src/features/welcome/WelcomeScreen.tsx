import { BrandMark } from "../../components/ui/BrandMark";
import { PrimaryButton } from "../../components/ui/PrimaryButton";
import { GhostButton } from "../../components/ui/GhostButton";

interface WelcomeScreenProps {
  onContinueWithoutLogin: () => void;
  onLogin: () => void;
}

/**
 * The one screen the spec calls out for maximum restraint: a headline, one line of
 * support copy, and exactly one dominant action. "Continuar sem login" is primary
 * because it's what this version of the product actually offers — "Entrar" is a
 * secondary, honest detour (see features/welcome/CloudLoginScreen.tsx), not a dead end.
 */
export function WelcomeScreen({ onContinueWithoutLogin, onLogin }: WelcomeScreenProps) {
  return (
    <div className="flex h-full min-h-screen w-full flex-col items-center justify-center gap-8 bg-app px-8 py-10 text-center">
      <BrandMark size={44} />

      <div className="flex max-w-xs flex-col gap-3">
        <h1 className="text-2xl font-semibold leading-tight tracking-tight text-neutral-50">
          Respostas melhores,
          <br />
          no momento certo.
        </h1>
        <p className="text-sm leading-relaxed text-neutral-400">
          O Helppye acompanha entrevistas e reuniões, entende a conversa e sugere o que responder.
        </p>
      </div>

      <div className="flex w-full max-w-xs flex-col items-center gap-2">
        <PrimaryButton fullWidth onClick={onContinueWithoutLogin}>
          Continuar sem login
        </PrimaryButton>
        <GhostButton onClick={onLogin}>Entrar</GhostButton>
      </div>
    </div>
  );
}
