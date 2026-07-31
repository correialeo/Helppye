import { BrandMark } from "../../components/ui/BrandMark";
import { GhostButton } from "../../components/ui/GhostButton";
import { PasswordInput } from "../../components/ui/PasswordInput";
import { PrimaryButton } from "../../components/ui/PrimaryButton";
import { TextInput } from "../../components/ui/TextInput";

interface CloudLoginScreenProps {
  onContinueWithoutLogin: () => void;
  onBack: () => void;
}

export function CloudLoginScreen({ onContinueWithoutLogin, onBack }: CloudLoginScreenProps) {
  return (
    <div className="flex h-full min-h-screen w-full items-center justify-center bg-black px-4 py-5">
      <div className="relative flex min-h-[540px] w-full max-w-[500px] flex-col items-center rounded-[9px] border border-white/16 bg-black px-8 py-12 shadow-[0_20px_70px_rgba(0,0,0,.7)]">
        <button
          type="button"
          aria-label="Voltar"
          onClick={onBack}
          className="absolute left-3 top-3 h-3 w-3 rounded-full bg-red-400"
        />
        <div className="mt-8 flex items-center gap-3">
          <BrandMark size={34} />
          <span className="text-[30px] font-semibold tracking-tight text-white">Helppye</span>
        </div>

        <div className="mt-20 flex w-full max-w-[296px] flex-col gap-4">
          <TextInput label="Email" placeholder="seu@email.com" type="email" />
          <PasswordInput label="Senha" placeholder="Senha" />
          <PrimaryButton
            fullWidth
            className="mt-1 rounded-full bg-white py-3 text-[13px] font-bold text-black shadow-none hover:bg-neutral-200 active:bg-neutral-300"
            onClick={onContinueWithoutLogin}
          >
            Entrar
          </PrimaryButton>
        </div>

        <div className="mt-4 flex flex-col items-center gap-4 text-xs text-neutral-400">
          <p>
            Nao tem uma conta?{" "}
            <button className="font-medium underline underline-offset-2" type="button" onClick={onContinueWithoutLogin}>
              Criar Conta
            </button>
          </p>
          <GhostButton onClick={onContinueWithoutLogin} className="p-0 text-xs underline underline-offset-2">
            Esqueceu a senha?
          </GhostButton>
        </div>

        <p className="mt-auto max-w-[270px] text-center text-[11px] leading-relaxed text-neutral-400">
          Ao continuar, voce concorda com os nossos{" "}
          <span className="font-semibold text-neutral-200 underline underline-offset-2">Termos de Servico</span> e{" "}
          <span className="font-semibold text-neutral-200 underline underline-offset-2">Politica de Privacidade</span>
        </p>
      </div>
    </div>
  );
}
