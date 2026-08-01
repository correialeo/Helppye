//! Camada de speech-to-text, desacoplada de captura e segmentação.
//!
//! Dois contratos, de propósito:
//!
//! - `provider::TranscriptionProvider` — o **ponto de extensão** pluggable, baseado em
//!   sessão (`start_session` → `push_audio` → `finish`/`cancel`). É o que a configuração
//!   seleciona, o que o registry indexa e o que declara capacidades.
//! - `segment_transcriber::SegmentTranscriber` — o contrato interno *segmento entra, texto
//!   sai*, que descreve um engine batch como o whisper.cpp. Não é selecionável nem visível
//!   fora daqui: `whisper_local::WhisperLocalTranscriptionProvider` o adapta ao contrato de
//!   sessão.
//!
//! `runtime::TranscriptionRuntime` é o dono do ciclo de vida: uma sessão de transcrição por
//! fonte por sessão de conversa, descarte de evento obsoleto no backend e normalização
//! determinística antes de a fala chegar à Conversation Timeline.
//!
//! `queue::TranscriptionQueue` continua sendo o ponto de entrega limitado e não bloqueante
//! entre a segmentação e esta camada — ele só passou a alimentar o runtime em vez de chamar
//! o transcritor direto.

pub mod error;
pub mod events;
pub mod provider;
pub mod queue;
pub mod registry;
pub mod runtime;
pub mod segment_transcriber;
pub mod session;
pub mod settings;
pub mod types;
pub mod whisper_local;
pub mod whisper_provider;

#[cfg(test)]
pub mod fake_provider;

use std::sync::Arc;

use serde::Serialize;
use tauri::State;
use tracing::info;

use crate::normalization::{
    DeterministicNormalizer, TranscriptCorrectionMode, TranscriptionVocabulary, VocabularyEntry,
};
use queue::TranscriptionQueue;
use registry::{TranscriptionProviderDescriptor, TranscriptionProviderRegistry};
use runtime::{TranscriptionRuntime, TranscriptionRuntimeStats};
use segment_transcriber::SegmentTranscriber;
use settings::TranscriptionSettings;
use types::{InferenceDevice, ModelConfig, TranscriptionLanguage};

/// Tauri event name for `types::TranscriptEvent`, mirroring `audio::CAPTURE_EVENT`.
pub const TRANSCRIPTION_EVENT: &str = "transcription://event";

/// Managed Tauri state da camada de transcrição.
///
/// `transcriber` continua sendo o engine batch local: é ele que `model_manager` carrega, e
/// carregar modelo é uma operação do engine, não da sessão. `runtime` é quem o resto do app
/// usa para transcrever de fato.
pub struct TranscriptionState {
    pub transcriber: Arc<dyn SegmentTranscriber>,
    pub queue: Arc<TranscriptionQueue>,
    pub runtime: Arc<TranscriptionRuntime>,
    pub registry: TranscriptionProviderRegistry,
    loaded_model_name: tokio::sync::Mutex<Option<String>>,
    /// Vocabulário vigente. Vive aqui, e não dentro do normalizador, porque o runtime guarda
    /// o normalizador atrás de `Arc<dyn TranscriptNormalizer>` — um trait object imutável,
    /// de propósito: trocar vocabulário no meio de uma normalização em curso produziria
    /// resultados diferentes para segmentos da mesma fala. Acrescentar um termo reconstrói o
    /// normalizador inteiro e o instala; a normalização em voo termina com o vocabulário
    /// antigo, a próxima já usa o novo.
    vocabulary: std::sync::Mutex<TranscriptionVocabulary>,
}

impl TranscriptionState {
    pub fn new(
        transcriber: Arc<dyn SegmentTranscriber>,
        queue: Arc<TranscriptionQueue>,
        runtime: Arc<TranscriptionRuntime>,
        registry: TranscriptionProviderRegistry,
    ) -> Self {
        TranscriptionState {
            transcriber,
            queue,
            runtime,
            registry,
            loaded_model_name: tokio::sync::Mutex::new(None),
            vocabulary: std::sync::Mutex::new(TranscriptionVocabulary::default()),
        }
    }
}

/// Loads (or reloads) the transcription model. No default `model_path` — an empty/missing
/// path is a configuration error, never a trigger to fetch a model automatically. `language`
/// of `None` or empty selects `TranscriptionLanguage::Automatic`; otherwise it's fixed to the
/// given tag (e.g. `"pt"`), the default the frontend offers.
#[tauri::command]
pub async fn configure_transcription_command(
    state: State<'_, TranscriptionState>,
    model_path: String,
    model_name: String,
    language: Option<String>,
) -> Result<(), String> {
    let language = match language {
        Some(tag) if !tag.is_empty() => TranscriptionLanguage::Fixed(tag),
        _ => TranscriptionLanguage::Automatic,
    };
    let config = ModelConfig {
        model_path: model_path.into(),
        model_name,
        language: language.clone(),
        device: InferenceDevice::Cpu,
    };
    let model_name = config.model_name.clone();
    state
        .transcriber
        .load(config)
        .await
        .map_err(|e| e.to_string())?;
    info!(
        provider = state.transcriber.provider_name(),
        model_name, "transcription model loaded"
    );

    // A configuração da camada acompanha o que foi carregado, para que uma sessão nova use
    // o idioma/modelo que o usuário acabou de escolher sem depender de outra chamada.
    let mut settings = state.runtime.settings();
    settings.language = language.into();
    settings.model = Some(model_name.clone());
    state.runtime.set_settings(settings);

    *state.loaded_model_name.lock().await = Some(model_name);
    Ok(())
}

/// Snapshot of transcription-layer health for a dev-mode diagnostics panel — never affects
/// capture or transcription behavior, purely informational.
#[derive(Debug, Clone, Serialize)]
pub struct TranscriptionDiagnostics {
    pub provider_name: &'static str,
    pub provider_id: provider::TranscriptionProviderId,
    pub loaded_model_name: Option<String>,
    pub dropped_segments: u64,
    #[serde(flatten)]
    pub queue_metrics: queue::TranscriptionQueueMetrics,
    #[serde(flatten)]
    pub runtime: TranscriptionRuntimeStats,
    /// Providers efetivamente registrados neste processo — diferente de
    /// `transcription_providers_command`, que lista também os previstos e indisponíveis.
    pub registered_providers: Vec<provider::TranscriptionProviderId>,
    /// Sessão de transcrição viva por fonte. Duas entradas com o mesmo id seria um bug de
    /// isolamento; por isso o número aparece cru.
    pub active_transcription_sessions: Vec<(crate::audio::types::AudioSource, u64)>,
    pub correction_mode: TranscriptCorrectionMode,
    pub contextual_corrector_registered: bool,
}

#[tauri::command]
pub async fn transcription_diagnostics_command(
    state: State<'_, TranscriptionState>,
) -> Result<TranscriptionDiagnostics, String> {
    Ok(TranscriptionDiagnostics {
        provider_name: state.transcriber.provider_name(),
        provider_id: state.runtime.provider_id(),
        loaded_model_name: state.loaded_model_name.lock().await.clone(),
        dropped_segments: state.queue.dropped_segments(),
        queue_metrics: state.queue.metrics(),
        runtime: state.runtime.stats(),
        registered_providers: state.registry.registered_ids(),
        active_transcription_sessions: state
            .runtime
            .active_transcription_sessions()
            .into_iter()
            .map(|(source, id)| (source, id.0))
            .collect(),
        correction_mode: state.runtime.correction_mode(),
        contextual_corrector_registered: state.runtime.has_contextual_corrector(),
    })
}

/// Modo de correção vigente. Exposto só atrás de "Modo de desenvolvedor": trocar isto muda o
/// texto que chega à timeline, e não é uma escolha de uso normal.
#[tauri::command]
pub async fn transcription_correction_mode_command(
    state: State<'_, TranscriptionState>,
) -> Result<TranscriptCorrectionMode, String> {
    Ok(state.runtime.correction_mode())
}

#[tauri::command]
pub async fn transcription_set_correction_mode_command(
    state: State<'_, TranscriptionState>,
    mode: TranscriptCorrectionMode,
) -> Result<(), String> {
    state.runtime.set_correction_mode(mode);
    Ok(())
}

/// Vocabulário técnico vigente, na forma em que a UI pode listá-lo e editá-lo.
#[tauri::command]
pub async fn transcription_vocabulary_command(
    state: State<'_, TranscriptionState>,
) -> Result<Vec<VocabularyEntry>, String> {
    Ok(state
        .vocabulary
        .lock()
        .map_err(|_| "vocabulary mutex poisoned".to_string())?
        .entries()
        .to_vec())
}

/// Acrescenta (ou substitui) um termo do vocabulário e reinstala o normalizador. Um
/// `canonical` vazio é recusado: sem forma canônica não há para o que normalizar.
#[tauri::command]
pub async fn transcription_add_vocabulary_entry_command(
    state: State<'_, TranscriptionState>,
    canonical: String,
    aliases: Vec<String>,
) -> Result<Vec<VocabularyEntry>, String> {
    if canonical.trim().is_empty() {
        return Err("termo canônico não pode ser vazio".to_string());
    }
    let entries = {
        let mut vocabulary = state
            .vocabulary
            .lock()
            .map_err(|_| "vocabulary mutex poisoned".to_string())?;
        vocabulary.add_entry(VocabularyEntry {
            canonical: canonical.trim().to_string(),
            aliases: aliases
                .into_iter()
                .map(|a| a.trim().to_string())
                .filter(|a| !a.is_empty())
                .collect(),
        });
        vocabulary.entries().to_vec()
    };
    state
        .runtime
        .set_normalizer(Arc::new(DeterministicNormalizer::new(
            TranscriptionVocabulary::from_entries(entries.clone()),
        )));
    Ok(entries)
}

/// Backends disponíveis **e** previstos, com as capacidades declaradas por cada um. A UI usa
/// isto em vez de assumir o que um provider faz.
#[tauri::command]
pub async fn transcription_providers_command(
    state: State<'_, TranscriptionState>,
) -> Result<Vec<TranscriptionProviderDescriptor>, String> {
    Ok(state.registry.descriptors())
}

#[tauri::command]
pub async fn transcription_settings_command(
    state: State<'_, TranscriptionState>,
) -> Result<TranscriptionSettings, String> {
    Ok(state.runtime.settings())
}

/// Troca o provider de transcrição. Falha se o provider selecionado não estiver registrado
/// — nunca cai em outro backend silenciosamente.
#[tauri::command]
pub async fn transcription_set_settings_command(
    state: State<'_, TranscriptionState>,
    settings: TranscriptionSettings,
) -> Result<(), String> {
    let provider = state
        .registry
        .get(settings.provider)
        .map_err(|e| e.to_string())?;
    // Um provider registrado ainda pode estar impedido de rodar (modelo não carregado,
    // credencial ausente). Perguntar antes de trocar transforma "toda transcrição falha em
    // silêncio a partir de agora" numa mensagem de erro na hora da escolha.
    provider.readiness().await.map_err(|e| e.to_string())?;
    state.runtime.set_provider(provider);
    state.runtime.set_settings(settings);
    Ok(())
}
