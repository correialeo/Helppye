//! `whisper-rs` (whisper.cpp bindings) implementation of `TranscriptionProvider`. See
//! `docs/local-transcription.md` for the evaluation behind this choice, including the
//! open gap on Windows build verification.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::audio::segment::AudioSegment;
use crate::transcription::error::TranscriptionError;
use crate::transcription::provider::TranscriptionProvider;
use crate::transcription::types::{
    InferenceDevice, ModelConfig, Transcript, TranscriptionLanguage,
};

/// whisper.cpp expects mono f32 PCM at this rate. The shared capture pipeline already
/// resamples every source to this target (`CaptureConfig`), so a mismatch here means an
/// upstream invariant was violated, not something for this provider to silently fix.
const WHISPER_SAMPLE_RATE: u32 = 16_000;

struct LoadedModel {
    context: Arc<WhisperContext>,
    language: TranscriptionLanguage,
}

/// `load` builds the model once; `transcribe` creates a fresh `WhisperState` per call
/// (cheap relative to inference) so concurrent segments never contend on shared mutable
/// inference state, while the loaded weights (`WhisperContext`, `Send + Sync`) are reused
/// across every call — satisfies the load-once-and-reuse requirement in
/// `docs/local-transcription.md`.
#[derive(Default)]
pub struct WhisperCppProvider {
    model: RwLock<Option<LoadedModel>>,
}

impl WhisperCppProvider {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl TranscriptionProvider for WhisperCppProvider {
    async fn load(&self, config: ModelConfig) -> Result<(), TranscriptionError> {
        if config.device == InferenceDevice::Gpu {
            return Err(TranscriptionError::Internal(
                "GPU inference requested but this build has no GPU backend compiled in \
                 (default whisper-rs features are CPU-only, see docs/local-transcription.md section 4)"
                    .into(),
            ));
        }

        if !config.model_path.is_file() {
            return Err(TranscriptionError::ModelNotFound(
                config.model_path.display().to_string(),
            ));
        }

        let model_path = config.model_path.clone();
        let context = tokio::task::spawn_blocking(move || {
            WhisperContext::new_with_params(&model_path, WhisperContextParameters::default())
        })
        .await
        .map_err(|e| TranscriptionError::Internal(format!("model load task panicked: {e}")))?
        .map_err(|e| TranscriptionError::ModelLoadFailed(e.to_string()))?;

        *self.model.write().await = Some(LoadedModel {
            context: Arc::new(context),
            language: config.language,
        });
        Ok(())
    }

    async fn transcribe(&self, segment: AudioSegment) -> Result<Transcript, TranscriptionError> {
        if segment.sample_rate != WHISPER_SAMPLE_RATE {
            return Err(TranscriptionError::Internal(format!(
                "whisper-cpp requires {WHISPER_SAMPLE_RATE}Hz mono input, got {}Hz",
                segment.sample_rate
            )));
        }

        let (context, language) = {
            let guard = self.model.read().await;
            let loaded = guard.as_ref().ok_or(TranscriptionError::NotConfigured)?;
            (Arc::clone(&loaded.context), loaded.language.clone())
        };

        let segment_id = segment.id;
        let source = segment.source;
        let started_at = segment.started_at;
        let ended_at = segment.ended_at;
        let samples = segment.samples;

        let start = std::time::Instant::now();
        let (text, detected_language) =
            tokio::task::spawn_blocking(move || transcribe_blocking(&context, &language, &samples))
                .await
                .map_err(|e| {
                    TranscriptionError::Internal(format!("transcription task panicked: {e}"))
                })??;

        Ok(Transcript {
            segment_id,
            source,
            text,
            language: detected_language,
            started_at,
            ended_at,
            processing_time_ms: start.elapsed().as_millis() as u64,
        })
    }

    fn provider_name(&self) -> &'static str {
        "whisper-cpp"
    }
}

/// O whisper anota trechos *sem fala* — música, silêncio, aplausos, ruído — em vez de
/// devolver texto vazio: `[Música]`, `[BLANK_AUDIO]`, `[Aplausos]`, `♪`. Isso não é fala e
/// não pode virar uma utterance. Uma marcação dessas logo depois de uma pergunta real abre
/// uma utterance nova no mesmo turno, que (a) cancela a geração já em andamento para a
/// pergunta e (b) é então corretamente classificada como `[SKIP]` — o usuário vê "Nenhuma
/// sugestão" exatamente na fala que mais precisava de resposta.
///
/// Só colchetes e notas musicais são removidos em qualquer posição do texto: o whisper
/// nunca envolve fala real em colchetes. Parênteses aparecem ocasionalmente em fala
/// transcrita de verdade, então `(...)` só é descartado quando é o segmento inteiro.
fn strip_non_speech_annotations(text: &str) -> String {
    let trimmed = text.trim();
    if let Some(inner) = trimmed
        .strip_prefix('(')
        .and_then(|rest| rest.strip_suffix(')'))
    {
        if !inner.contains('(') && !inner.contains(')') {
            return String::new();
        }
    }

    let mut out = String::with_capacity(trimmed.len());
    let mut depth = 0usize;
    for ch in trimmed.chars() {
        match ch {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            '♪' | '♫' => {}
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }

    // Colapsa o espaço que sobra onde a marcação estava, para não deixar espaços duplos
    // atravessarem até a junção de texto da timeline.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Runs on a blocking task: `WhisperState::full` is a synchronous, CPU-bound call and
/// must never run on the async runtime's worker threads.
fn transcribe_blocking(
    context: &WhisperContext,
    language: &TranscriptionLanguage,
    samples: &[f32],
) -> Result<(String, Option<String>), TranscriptionError> {
    let mut state = context.create_state().map_err(|e| {
        TranscriptionError::Internal(format!("failed to create inference state: {e}"))
    })?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);

    let language_tag = match language {
        TranscriptionLanguage::Automatic => None,
        TranscriptionLanguage::Fixed(tag) => Some(tag.as_str()),
    };
    params.set_language(language_tag);

    state
        .full(params, samples)
        .map_err(|e| TranscriptionError::InferenceFailed(e.to_string()))?;

    let num_segments = state.full_n_segments();
    let mut text = String::new();
    for i in 0..num_segments {
        let Some(segment) = state.get_segment(i) else {
            continue;
        };
        let segment_text = segment
            .to_str_lossy()
            .map_err(|e| TranscriptionError::InferenceFailed(e.to_string()))?;
        let cleaned = strip_non_speech_annotations(&segment_text);
        if cleaned.is_empty() {
            continue;
        }
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(&cleaned);
    }

    let detected_language = match language {
        TranscriptionLanguage::Fixed(tag) => Some(tag.clone()),
        TranscriptionLanguage::Automatic => {
            let lang_id = state.full_lang_id_from_state();
            whisper_rs::get_lang_str(lang_id).map(str::to_string)
        }
    };

    Ok((text, detected_language))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::segment::AudioTimestamp;
    use crate::audio::types::AudioSource;

    fn config_with_missing_model() -> ModelConfig {
        ModelConfig {
            model_path: "/nonexistent/ggml-does-not-exist.bin".into(),
            model_name: "test".into(),
            language: TranscriptionLanguage::default(),
            device: InferenceDevice::Cpu,
        }
    }

    #[test]
    fn non_speech_annotations_are_dropped_entirely() {
        for annotation in [
            "[Música]",
            "[MÚSICA]",
            "[BLANK_AUDIO]",
            "[Aplausos]",
            "(música de fundo)",
            "♪",
            "♪♪♪",
            "   ",
        ] {
            assert_eq!(
                strip_non_speech_annotations(annotation),
                "",
                "{annotation:?} não é fala e não pode virar uma utterance"
            );
        }
    }

    #[test]
    fn annotation_glued_to_real_speech_leaves_the_speech_intact() {
        // O caso observado: a pergunta e a marcação chegam no mesmo texto. Descartar o
        // segmento inteiro perderia a pergunta; manter a marcação suja o prompt.
        assert_eq!(
            strip_non_speech_annotations(
                "Me conta um caso real em que você usou monolito. [Música]"
            ),
            "Me conta um caso real em que você usou monolito."
        );
        assert_eq!(
            strip_non_speech_annotations("[Música] Então, o que você acha?"),
            "Então, o que você acha?"
        );
    }

    #[test]
    fn real_speech_with_parentheses_is_preserved() {
        // Parênteses no meio de fala real são fala; só o segmento inteiro entre parênteses
        // é anotação.
        assert_eq!(
            strip_non_speech_annotations("A latência (p99) subiu muito."),
            "A latência (p99) subiu muito."
        );
    }

    #[tokio::test]
    async fn load_fails_cleanly_on_missing_model_file() {
        let provider = WhisperCppProvider::new();
        let err = provider
            .load(config_with_missing_model())
            .await
            .unwrap_err();
        assert!(matches!(err, TranscriptionError::ModelNotFound(_)));
    }

    #[tokio::test]
    async fn load_rejects_gpu_device_without_gpu_features() {
        let provider = WhisperCppProvider::new();
        let mut config = config_with_missing_model();
        config.device = InferenceDevice::Gpu;
        let err = provider.load(config).await.unwrap_err();
        assert!(matches!(err, TranscriptionError::Internal(_)));
    }

    #[tokio::test]
    async fn transcribe_without_load_is_not_configured() {
        let provider = WhisperCppProvider::new();
        let segment = AudioSegment::new(
            AudioSource::SystemOutput,
            vec![0.0; 16_000],
            WHISPER_SAMPLE_RATE,
            AudioTimestamp(0),
            AudioTimestamp(1_000),
        );
        let err = provider.transcribe(segment).await.unwrap_err();
        assert!(matches!(err, TranscriptionError::NotConfigured));
    }

    #[tokio::test]
    async fn transcribe_rejects_wrong_sample_rate() {
        let provider = WhisperCppProvider::new();
        let segment = AudioSegment::new(
            AudioSource::Microphone,
            vec![0.0; 8_000],
            8_000,
            AudioTimestamp(0),
            AudioTimestamp(1_000),
        );
        let err = provider.transcribe(segment).await.unwrap_err();
        assert!(matches!(err, TranscriptionError::Internal(_)));
    }

    #[test]
    fn provider_name_identifies_backend() {
        let provider = WhisperCppProvider::new();
        assert_eq!(provider.provider_name(), "whisper-cpp");
    }
}
