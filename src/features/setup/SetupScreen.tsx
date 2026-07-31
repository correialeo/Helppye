import { useState, type ReactNode } from "react";
import { Check, ChevronDown, ChevronUp, KeyRound, Mic, MonitorSpeaker, Sparkles, Volume2 } from "lucide-react";
import { InlineNotice } from "../../components/ui/InlineNotice";
import { PrimaryButton } from "../../components/ui/PrimaryButton";
import { SecondaryButton } from "../../components/ui/SecondaryButton";
import { ProviderOption } from "../../components/ui/ProviderOption";
import { OllamaPanel } from "../ai-provider/OllamaPanel";
import { CloudProviderPanel, isCloudProvider } from "../ai-provider/CloudProviderPanel";
import { useAudioCapture } from "../../hooks/useAudioCapture";
import { useModelStatus } from "../../hooks/useModelStatus";
import { useResponseProvider } from "../../hooks/useResponseProvider";
import type { ResponseProviderKind } from "../../types/responseProvider";

interface SetupScreenProps {
  onBack: () => void;
  onComplete: () => void;
}

const PROVIDERS: { value: ResponseProviderKind; name: string; description: string; badge?: string }[] = [
  { value: "ollama", name: "Ollama", description: "Local e privado", badge: "Recomendado" },
  { value: "open_ai", name: "OpenAI", description: "Rapido e preciso" },
  { value: "anthropic", name: "Anthropic", description: "Respostas naturais" },
  { value: "deep_seek", name: "DeepSeek", description: "Alternativa em nuvem" },
];

function SetupItem({
  index,
  title,
  description,
  done,
  optional,
  open,
  onToggle,
  children,
}: {
  index: number;
  title: string;
  description: string;
  done?: boolean;
  optional?: boolean;
  open: boolean;
  onToggle: () => void;
  children?: ReactNode;
}) {
  return (
    <section className="overflow-hidden rounded-[8px] border border-white/12 bg-[#080809]">
      <button type="button" onClick={onToggle} className="flex w-full items-center gap-4 px-4 py-3 text-left">
        <span
          className={`grid h-8 w-8 shrink-0 place-items-center rounded-full border text-xs font-bold ${
            done
              ? "border-green-400/30 bg-green-400/12 text-green-300"
              : "border-brand-400/30 bg-brand-500/12 text-brand-300"
          }`}
        >
          {done ? <Check className="h-4 w-4" /> : String(index).padStart(2, "0")}
        </span>
        <span className="min-w-0 flex-1">
          <span className="flex items-center gap-2 text-sm font-bold text-white">
            {title}
            {optional && (
              <span className="rounded-full bg-white/12 px-1.5 py-0.5 text-[10px] font-bold uppercase text-white/62">
                Opcional
              </span>
            )}
          </span>
          <span className="block truncate text-xs text-white/62">{description}</span>
        </span>
        {open ? <ChevronUp className="h-4 w-4 text-white/62" /> : <ChevronDown className="h-4 w-4 text-white/62" />}
      </button>
      {open && <div className="border-t border-white/8 px-16 pb-5 pt-3">{children}</div>}
    </section>
  );
}

export function SetupScreen({ onBack, onComplete }: SetupScreenProps) {
  const [open, setOpen] = useState(1);
  const microphone = useAudioCapture("microphone");
  const systemOutput = useAudioCapture("system_output");
  const model = useModelStatus();
  const { status, saveConfig, saveApiKey, removeApiKey } = useResponseProvider();
  const [selectedProvider, setSelectedProvider] = useState<ResponseProviderKind | null>(null);
  const activeProvider = selectedProvider ?? status?.provider ?? "ollama";

  const permissionsReady = microphone.status.kind === "capturing" && systemOutput.status.kind === "capturing";
  const modelReady = model.status?.state.state === "ready";

  const selectProvider = async (provider: ResponseProviderKind) => {
    setSelectedProvider(provider);
    if (provider === "ollama" && status?.provider !== "ollama") {
      await saveConfig({ provider: "ollama", model: "llama3.1", baseUrl: "http://localhost:11434", ollamaKeepAlive: "5m" });
    }
  };

  return (
    <div className="flex h-full min-h-screen w-full flex-col bg-black px-8 py-7 text-white">
      <header className="flex items-center justify-between">
        <div className="flex items-center gap-2 text-xs text-white/78">
          <Sparkles className="h-4 w-4" />
          Helppye Setup
        </div>
        <button type="button" onClick={onBack} className="text-xs font-semibold text-white/48 hover:text-white/80">
          Voltar
        </button>
      </header>

      <main className="mx-auto flex w-full max-w-[732px] flex-1 flex-col py-12">
        <div className="mb-8 h-1 w-11 self-center rounded-full bg-white/18" />
        <p className="text-[11px] font-bold uppercase tracking-wide text-white/52">Onboarding de configuracoes</p>
        <h1 className="mt-1 text-[22px] font-bold leading-tight">Conclua a configuracao em ordem</h1>
        <p className="mt-3 max-w-[720px] text-sm leading-relaxed text-white/72">
          Conclua primeiro as verificacoes obrigatorias e depois escolha as opcoes de audio, transcricao, analise e modelo
          que combinam com a forma como voce quer usar o Helppye.
        </p>

        <div className="mt-5 flex flex-col gap-2">
          <SetupItem
            index={1}
            title="Verificacao de permissoes"
            description={permissionsReady ? "O acesso obrigatorio do sistema esta pronto." : "Libere microfone e audio do sistema."}
            done={permissionsReady}
            open={open === 1}
            onToggle={() => setOpen(open === 1 ? 0 : 1)}
          >
            <div className="flex flex-col gap-2">
              <div className="rounded-[8px] border border-green-400/20 bg-green-400/8 px-4 py-3">
                <div className="flex items-center gap-3">
                  <Mic className="h-4 w-4 text-green-300" />
                  <div className="flex-1">
                    <p className="text-sm font-bold">Microfone</p>
                    <p className="text-xs text-white/62">Permite captura de voz e transcricao em tempo real.</p>
                  </div>
                  <SecondaryButton className="px-3 py-1.5 text-xs" onClick={microphone.start}>
                    {microphone.status.kind === "capturing" ? "Pronto" : "Permitir"}
                  </SecondaryButton>
                </div>
              </div>
              <div className="rounded-[8px] border border-green-400/20 bg-green-400/8 px-4 py-3">
                <div className="flex items-center gap-3">
                  <MonitorSpeaker className="h-4 w-4 text-green-300" />
                  <div className="flex-1">
                    <p className="text-sm font-bold">Gravacao de tela</p>
                    <p className="text-xs text-white/62">Permite ler o contexto de audio quando voce pede ajuda.</p>
                  </div>
                  <SecondaryButton className="px-3 py-1.5 text-xs" onClick={systemOutput.start}>
                    {systemOutput.status.kind === "capturing" ? "Pronto" : "Permitir"}
                  </SecondaryButton>
                </div>
              </div>
            </div>
          </SetupItem>

          <SetupItem
            index={2}
            title="Transcricao"
            description={modelReady ? "Reconhecimento local pronto." : "Prepare o modelo local de transcricao."}
            done={modelReady}
            open={open === 2}
            onToggle={() => setOpen(open === 2 ? 0 : 2)}
          >
            <div className="flex items-center justify-between gap-3 rounded-[8px] border border-white/10 bg-[#111112] px-4 py-3">
              <div>
                <p className="text-sm font-bold">{model.status?.display_name ?? "Modelo local"}</p>
                <p className="text-xs text-white/58">{modelReady ? "Instalado e verificado." : "Necessario para transcrever sem nuvem."}</p>
              </div>
              {!modelReady && (
                <SecondaryButton onClick={model.startDownload}>
                  Preparar
                </SecondaryButton>
              )}
            </div>
            {model.error && <InlineNotice tone="error">{model.error}</InlineNotice>}
          </SetupItem>

          <SetupItem
            index={3}
            title="Analise de texto e imagem"
            description="Conecte o provedor que vai gerar sugestoes."
            done={Boolean(status)}
            open={open === 3}
            onToggle={() => setOpen(open === 3 ? 0 : 3)}
          >
            <div className="flex flex-col gap-2">
              {PROVIDERS.map((provider) => (
                <ProviderOption
                  key={provider.value}
                  name={provider.name}
                  description={provider.description}
                  badge={provider.badge}
                  selected={activeProvider === provider.value}
                  onSelect={() => selectProvider(provider.value)}
                />
              ))}
            </div>
          </SetupItem>

          <SetupItem
            index={4}
            title="Provedores de IA"
            description="Modelos locais e provedores em nuvem ficam disponiveis nas configuracoes."
            optional
            open={open === 4}
            onToggle={() => setOpen(open === 4 ? 0 : 4)}
          >
            {status && activeProvider === "ollama" && (
              <OllamaPanel status={status} onSave={(config) => saveConfig({ provider: "ollama", ...config })} />
            )}
            {status && isCloudProvider(activeProvider) && (
              <CloudProviderPanel
                provider={activeProvider}
                status={status}
                onSaveKey={async (provider, apiKey, modelName) => {
                  await saveConfig({ provider, model: modelName, baseUrl: null, ollamaKeepAlive: null });
                  await saveApiKey(provider, apiKey);
                }}
                onRemoveKey={removeApiKey}
              />
            )}
          </SetupItem>

          <SetupItem
            index={5}
            title="Selecao de modelo"
            description="Abra as configuracoes de modelos para escolher os padroes que preferir."
            open={open === 5}
            onToggle={() => setOpen(open === 5 ? 0 : 5)}
          >
            <div className="flex items-center gap-3 rounded-[8px] border border-white/10 bg-[#111112] px-4 py-3">
              <KeyRound className="h-4 w-4 text-white/42" />
              <p className="text-sm text-white/70">O modelo atual sera usado nas respostas da sessao.</p>
            </div>
          </SetupItem>

          <SetupItem
            index={6}
            title="Continuar gratis por 7 dias"
            description="O Helppye local pode ser usado sem conta nesta versao."
            open={open === 6}
            onToggle={() => setOpen(open === 6 ? 0 : 6)}
          >
            <div className="flex items-center gap-3 rounded-[8px] border border-white/10 bg-[#111112] px-4 py-3">
              <Volume2 className="h-4 w-4 text-white/42" />
              <p className="text-sm text-white/70">Nenhum cartao e necessario para continuar com os recursos locais.</p>
            </div>
          </SetupItem>
        </div>
      </main>

      <footer className="mx-auto flex w-full max-w-[732px] justify-end border-t border-white/8 pt-4">
        <PrimaryButton onClick={onComplete}>Concluir configuracao</PrimaryButton>
      </footer>
    </div>
  );
}
