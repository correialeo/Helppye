import { OnboardingLayout } from "../../components/layout/OnboardingLayout";
import { ProgressBar } from "../../components/feedback/ProgressBar";
import { InlineNotice } from "../../components/ui/InlineNotice";
import { GhostButton } from "../../components/ui/GhostButton";
import { ONBOARDING_STEPS, onboardingStepIndex } from "../../app/appFlow";
import { formatBytes, formatSeconds } from "../../utils/format";
import type { useModelStatus } from "../../hooks/useModelStatus";

type ModelStatusHook = ReturnType<typeof useModelStatus>;

/**
 * Shown in place of the live audio test until the local transcription model is ready.
 * Folded into the audio-setup step (rather than a separate top-level screen) because
 * that's the first point in the flow where transcription actually matters — and because
 * downloading only ever starts from an explicit click here, never silently on app
 * launch (see CLAUDE.md).
 */
export function ModelPrepareStep({ model }: { model: ModelStatusHook }) {
  const { status, error, progress, startDownload, cancelDownload } = model;
  const step = onboardingStepIndex("audio-setup");

  if (error) {
    return (
      <OnboardingLayout step={step} totalSteps={ONBOARDING_STEPS.length} title="Não foi possível verificar o modelo local">
        <InlineNotice tone="error">{error}</InlineNotice>
      </OnboardingLayout>
    );
  }

  if (!status) {
    return (
      <OnboardingLayout step={step} totalSteps={ONBOARDING_STEPS.length} title="Preparando reconhecimento de fala">
        <p className="text-sm text-neutral-500">Verificando...</p>
      </OnboardingLayout>
    );
  }

  const state = status.state.state;

  if (state === "downloading" || state === "verifying" || state === "installing") {
    const downloaded = progress?.downloaded ?? 0;
    const total = progress?.total ?? status.approximate_size_bytes;
    const percent = total > 0 ? Math.min(100, Math.round((downloaded / total) * 100)) : 0;
    const remainingSeconds =
      progress && progress.bytesPerSecond > 0 ? Math.max(0, total - downloaded) / progress.bytesPerSecond : NaN;

    return (
      <OnboardingLayout
        step={step}
        totalSteps={ONBOARDING_STEPS.length}
        title="Preparando reconhecimento de fala local"
        description={state === "downloading" ? "Isso só acontece uma vez." : undefined}
        secondaryAction={state === "downloading" ? <GhostButton onClick={cancelDownload}>Cancelar</GhostButton> : undefined}
      >
        <ProgressBar percent={percent} />
        {state === "downloading" ? (
          <div className="flex flex-col gap-0.5 text-xs text-neutral-500">
            <span>
              {formatBytes(downloaded)} de {formatBytes(total)} · {percent}%
            </span>
            <span>Tempo restante: {formatSeconds(remainingSeconds)}</span>
          </div>
        ) : (
          <p className="text-xs text-neutral-500">
            {state === "verifying" ? "Verificando integridade do arquivo..." : "Instalando..."}
          </p>
        )}
      </OnboardingLayout>
    );
  }

  if (state === "failed" || state === "corrupted") {
    const reason = "reason" in status.state ? status.state.reason : "Erro desconhecido.";
    return (
      <OnboardingLayout
        step={step}
        totalSteps={ONBOARDING_STEPS.length}
        title="Não foi possível preparar o reconhecimento de fala"
        primaryLabel="Tentar novamente"
        onPrimary={startDownload}
      >
        <InlineNotice tone="error">{reason}</InlineNotice>
      </OnboardingLayout>
    );
  }

  // not_installed / checking / cancelled / ready-but-not-yet-refreshed.
  return (
    <OnboardingLayout
      step={step}
      totalSteps={ONBOARDING_STEPS.length}
      title="Vamos preparar o reconhecimento de fala"
      description="O Helppye transcreve localmente, no seu computador — o áudio nunca sai da sua máquina."
      primaryLabel="Baixar e continuar"
      onPrimary={startDownload}
    >
      <div className="flex flex-col gap-0.5 rounded-lg border border-white/10 bg-surface px-4 py-3">
        <p className="text-sm font-medium text-neutral-200">{status.display_name}</p>
        <p className="text-xs text-neutral-500">Tamanho aproximado: {formatBytes(status.approximate_size_bytes)}</p>
      </div>
    </OnboardingLayout>
  );
}
