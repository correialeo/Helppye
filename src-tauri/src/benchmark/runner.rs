//! Execução de um fixture contra um provider, e as métricas que saem disso.
//!
//! O caminho medido é o caminho real, na mesma ordem do pipeline:
//!
//! ```text
//! WAV → chunks de 100 ms → TranscriptionProvider → TranscriptionEvent::Final
//!     → TranscriptNormalizer → ConversationTimeline → ConversationUtterance
//! ```
//!
//! O áudio é entregue em chunks do mesmo tamanho que a captura usa
//! (`CaptureConfig::frame_duration_ms`) em vez de num bloco só, porque um provider de
//! streaming se comporta de forma diferente nos dois casos e medir o bloco único favoreceria
//! artificialmente o backend batch.
//!
//! **O que este harness não faz:** não simula rede, não estima qualidade semântica e não
//! roda o provedor de resposta. `estimated_cost_usd` vem de uma tabela de preço declarada
//! pelo operador (`CostModel`), não de uma consulta ao provedor — inventar preço seria pior
//! que não reportar nenhum.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::audio::segment::AudioTimestamp;
use crate::benchmark::fixtures::BenchmarkFixture;
use crate::benchmark::wav::{self, WavError};
use crate::conversation::{ConversationAssemblerConfig, ConversationTimeline};
use crate::normalization::{
    TranscriptNormalizationInput, TranscriptNormalizationResult, TranscriptNormalizer,
};
use crate::transcription::events::TranscriptionEvent;
use crate::transcription::provider::TranscriptionProvider;
use crate::transcription::session::{
    AudioChunk, TranscriptionSessionContext, TranscriptionSessionId,
};
use crate::transcription::settings::TranscriptionSettings;

/// Taxa e tamanho de chunk que a captura real produz. Constantes locais em vez de leitura de
/// `CaptureConfig` porque o harness não constrói o pipeline de captura — mas os valores têm
/// que acompanhar `audio::config::CaptureConfig::default()`.
const TARGET_SAMPLE_RATE: u32 = 16_000;
const CHUNK_DURATION_MS: u64 = 100;

#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error(transparent)]
    Audio(#[from] WavError),
    #[error("provider não abriu sessão: {0}")]
    SessionStart(String),
}

/// Preço declarado por quem roda o benchmark. `None` significa "não sei", e o relatório
/// mostra vazio — nunca zero, que seria indistinguível de "de graça".
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct CostModel {
    pub usd_per_audio_minute: Option<f64>,
}

impl CostModel {
    pub const FREE_LOCAL: CostModel = CostModel {
        usd_per_audio_minute: Some(0.0),
    };

    fn estimate(self, audio_ms: u64) -> Option<f64> {
        let rate = self.usd_per_audio_minute?;
        Some(rate * (audio_ms as f64 / 60_000.0))
    }
}

/// Tempos medidos com relógio monotônico. `real_time_factor` é a métrica que decide se um
/// backend serve para uso ao vivo: acima de 1.0 ele transcreve mais devagar do que a pessoa
/// fala e a defasagem cresce sem limite ao longo da reunião.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct BenchmarkLatencies {
    pub audio_duration_ms: u64,
    pub first_partial_ms: Option<u64>,
    pub first_final_ms: Option<u64>,
    pub total_ms: u64,
    pub real_time_factor: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BenchmarkCaseResult {
    pub fixture_id: String,
    pub provider: String,
    pub model: Option<String>,
    pub language: String,
    pub source: crate::audio::types::AudioSource,
    pub latencies: BenchmarkLatencies,
    pub expected_transcript: String,
    pub raw_transcript: String,
    pub normalized_transcript: String,
    /// Word error rate contra `expected_transcript`, em `0.0..`. Pode passar de 1.0 quando o
    /// provider produz mais texto do que o esperado (alucinação, repetição).
    pub word_error_rate: f64,
    /// Termos de `technical_vocabulary` que aparecem no texto normalizado.
    pub vocabulary_hits: Vec<String>,
    pub vocabulary_misses: Vec<String>,
    /// Média das confianças reportadas nos resultados finais. `None` quando o backend não
    /// reporta confiança.
    pub mean_confidence: Option<f32>,
    pub final_transcript_count: usize,
    pub partial_transcript_count: usize,
    pub normalization_change_count: usize,
    pub utterance_count: usize,
    pub errors: Vec<String>,
    pub estimated_cost_usd: Option<f64>,
}

/// Roda um fixture ponta a ponta. `audio_path` é resolvido pelo chamador
/// (`FixtureManifest::audio_path`) para que o runner não precise saber onde o manifesto mora.
pub async fn run_fixture(
    provider: &dyn TranscriptionProvider,
    settings: &TranscriptionSettings,
    normalizer: &dyn TranscriptNormalizer,
    fixture: &BenchmarkFixture,
    audio_path: &std::path::Path,
    cost: CostModel,
) -> Result<BenchmarkCaseResult, RunnerError> {
    let decoded = wav::read_wav(audio_path)?;
    let samples = wav::to_target_rate(&decoded, TARGET_SAMPLE_RATE);
    let audio_duration_ms = duration_ms(samples.len(), TARGET_SAMPLE_RATE);

    let collected: Arc<Mutex<Vec<(TranscriptionEvent, Instant)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let sink_events = Arc::clone(&collected);

    let timeline = ConversationTimeline::new(ConversationAssemblerConfig::default());
    let session_id = timeline.session_id();

    let context = TranscriptionSessionContext {
        session_id,
        transcription_session_id: TranscriptionSessionId::next(),
        source: fixture.source,
        language: settings.language.clone().into(),
        model: settings.model.clone(),
        sink: Arc::new(move |event| {
            sink_events
                .lock()
                .expect("benchmark sink mutex poisoned")
                .push((event, Instant::now()));
        }),
    };

    let started_at = Instant::now();
    let mut session = provider
        .start_session(context)
        .await
        .map_err(|e| RunnerError::SessionStart(e.to_string()))?;

    let samples_per_chunk = (TARGET_SAMPLE_RATE as u64 * CHUNK_DURATION_MS / 1000) as usize;
    let mut errors = Vec::new();
    let mut offset_ms = 0u64;
    for chunk in samples.chunks(samples_per_chunk.max(1)) {
        let chunk_ms = duration_ms(chunk.len(), TARGET_SAMPLE_RATE);
        let audio = AudioChunk {
            source: fixture.source,
            samples: chunk.to_vec(),
            sample_rate: TARGET_SAMPLE_RATE,
            started_at: AudioTimestamp(offset_ms),
            ended_at: AudioTimestamp(offset_ms + chunk_ms),
            segment_id: None,
        };
        if let Err(e) = session.push_audio(audio).await {
            errors.push(e.to_string());
        }
        offset_ms += chunk_ms;
    }

    if let Err(e) = session.finish().await {
        errors.push(e.to_string());
    }
    let total_ms = elapsed_ms(started_at);

    let events = std::mem::take(&mut *collected.lock().expect("benchmark sink mutex poisoned"));
    let mut first_partial_ms = None;
    let mut first_final_ms = None;
    let mut partial_transcript_count = 0usize;
    let mut finals = Vec::new();
    let mut confidences = Vec::new();

    for (event, at) in events {
        match event {
            TranscriptionEvent::Partial(_) => {
                partial_transcript_count += 1;
                first_partial_ms.get_or_insert_with(|| at.saturating_duration_since(started_at));
            }
            TranscriptionEvent::Final(final_transcript) => {
                first_final_ms.get_or_insert_with(|| at.saturating_duration_since(started_at));
                if let Some(confidence) = final_transcript.confidence {
                    confidences.push(confidence);
                }
                finals.push(final_transcript);
            }
            TranscriptionEvent::Error(e) => errors.push(e.message),
            TranscriptionEvent::SpeechStarted(_) | TranscriptionEvent::SpeechEnded(_) => {}
        }
    }

    let mut raw_parts = Vec::new();
    let mut normalized_parts = Vec::new();
    let mut normalization_change_count = 0usize;
    for transcript in &finals {
        let normalization: TranscriptNormalizationResult =
            normalizer.normalize(TranscriptNormalizationInput {
                raw_text: transcript.text.clone(),
                source: transcript.source,
                language: transcript.language.clone(),
                provider: transcript.provider,
            });
        normalization_change_count += normalization.change_count();
        raw_parts.push(normalization.raw_text.clone());
        normalized_parts.push(normalization.normalized_text.clone());
        timeline.ingest_normalized_transcript(transcript, &normalization);
    }
    timeline.flush();

    let raw_transcript = join_speech(&raw_parts);
    let normalized_transcript = join_speech(&normalized_parts);
    let (hits, misses) = vocabulary_coverage(&fixture.technical_vocabulary, &normalized_transcript);

    Ok(BenchmarkCaseResult {
        fixture_id: fixture.id.clone(),
        provider: provider.id().as_str().to_string(),
        model: settings.model.clone(),
        language: language_label(&settings.language),
        source: fixture.source,
        latencies: BenchmarkLatencies {
            audio_duration_ms,
            first_partial_ms: first_partial_ms.map(millis),
            first_final_ms: first_final_ms.map(millis),
            total_ms,
            real_time_factor: if audio_duration_ms == 0 {
                0.0
            } else {
                total_ms as f64 / audio_duration_ms as f64
            },
        },
        expected_transcript: fixture.expected_transcript.clone(),
        word_error_rate: word_error_rate(&fixture.expected_transcript, &normalized_transcript),
        raw_transcript,
        normalized_transcript,
        vocabulary_hits: hits,
        vocabulary_misses: misses,
        mean_confidence: if confidences.is_empty() {
            None
        } else {
            Some(confidences.iter().sum::<f32>() / confidences.len() as f32)
        },
        final_transcript_count: finals.len(),
        partial_transcript_count,
        normalization_change_count,
        utterance_count: timeline.snapshot().utterances.len(),
        errors,
        estimated_cost_usd: cost.estimate(audio_duration_ms),
    })
}

/// Word error rate clássico: distância de edição em nível de palavra dividida pelo número de
/// palavras de referência. A comparação ignora caixa e pontuação, porque punir "DDD" contra
/// "ddd" aqui duplicaria o que `vocabulary_hits` já mede melhor.
pub fn word_error_rate(expected: &str, actual: &str) -> f64 {
    let reference = tokens(expected);
    let hypothesis = tokens(actual);
    if reference.is_empty() {
        return if hypothesis.is_empty() { 0.0 } else { 1.0 };
    }

    // Levenshtein com duas linhas: o comprimento de uma transcrição de reunião torna a
    // matriz completa desnecessariamente cara em memória.
    let mut previous: Vec<usize> = (0..=hypothesis.len()).collect();
    let mut current = vec![0usize; hypothesis.len() + 1];
    for (i, reference_word) in reference.iter().enumerate() {
        current[0] = i + 1;
        for (j, hypothesis_word) in hypothesis.iter().enumerate() {
            let substitution = previous[j] + usize::from(reference_word != hypothesis_word);
            current[j + 1] = substitution.min(previous[j + 1] + 1).min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }

    previous[hypothesis.len()] as f64 / reference.len() as f64
}

/// Cobertura de vocabulário técnico, casada por palavra inteira e sem caixa. Um termo com
/// espaço ("Entity Framework") é casado como sequência.
fn vocabulary_coverage(terms: &[String], text: &str) -> (Vec<String>, Vec<String>) {
    let haystack = tokens(text);
    let mut hits = Vec::new();
    let mut misses = Vec::new();
    for term in terms {
        let needle = tokens(term);
        let found = !needle.is_empty()
            && haystack
                .windows(needle.len())
                .any(|window| window == needle.as_slice());
        if found {
            hits.push(term.clone());
        } else {
            misses.push(term.clone());
        }
    }
    (hits, misses)
}

fn tokens(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|word| {
            word.chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect::<String>()
        })
        .filter(|word| !word.is_empty())
        .collect()
}

fn join_speech(parts: &[String]) -> String {
    parts
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn language_label(language: &crate::transcription::settings::LanguageCode) -> String {
    match language {
        crate::transcription::settings::LanguageCode::Automatic => "auto".to_string(),
        crate::transcription::settings::LanguageCode::Fixed(tag) => tag.clone(),
    }
}

fn duration_ms(sample_count: usize, rate: u32) -> u64 {
    if rate == 0 {
        return 0;
    }
    (sample_count as u64 * 1000) / u64::from(rate)
}

fn elapsed_ms(since: Instant) -> u64 {
    millis(since.elapsed())
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalization::{DeterministicNormalizer, TranscriptionVocabulary};
    use crate::transcription::fake_provider::{FakeBehavior, FakeTranscriptionProvider};

    fn fixture(expected: &str, vocabulary: &[&str]) -> BenchmarkFixture {
        BenchmarkFixture {
            id: "caso".into(),
            audio: "caso.wav".into(),
            expected_transcript: expected.into(),
            technical_vocabulary: vocabulary.iter().map(|s| s.to_string()).collect(),
            source: crate::audio::types::AudioSource::SystemOutput,
            language: Default::default(),
            notes: None,
        }
    }

    /// WAV mono 16 kHz de `ms` milissegundos, gravado num arquivo temporário.
    fn temp_wav(ms: u64) -> std::path::PathBuf {
        let samples = (TARGET_SAMPLE_RATE as u64 * ms / 1000) as usize;
        let mut data = Vec::new();
        for i in 0..samples {
            let value = ((i % 100) as i16) * 100;
            data.extend_from_slice(&value.to_le_bytes());
        }

        let mut fmt = Vec::new();
        fmt.extend_from_slice(&1u16.to_le_bytes());
        fmt.extend_from_slice(&1u16.to_le_bytes());
        fmt.extend_from_slice(&TARGET_SAMPLE_RATE.to_le_bytes());
        fmt.extend_from_slice(&(TARGET_SAMPLE_RATE * 2).to_le_bytes());
        fmt.extend_from_slice(&2u16.to_le_bytes());
        fmt.extend_from_slice(&16u16.to_le_bytes());

        let mut body = Vec::new();
        body.extend_from_slice(b"WAVE");
        body.extend_from_slice(b"fmt ");
        body.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
        body.extend_from_slice(&fmt);
        body.extend_from_slice(b"data");
        body.extend_from_slice(&(data.len() as u32).to_le_bytes());
        body.extend_from_slice(&data);

        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);

        let path = std::env::temp_dir().join(format!(
            "helppye-benchmark-{}.wav",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, out).unwrap();
        path
    }

    #[test]
    fn word_error_rate_is_zero_for_an_exact_match_ignoring_case_and_punctuation() {
        assert_eq!(word_error_rate("Usamos DDD aqui.", "usamos ddd aqui"), 0.0);
    }

    #[test]
    fn word_error_rate_counts_substitutions_insertions_and_deletions() {
        assert!((word_error_rate("um dois tres", "um dois") - 1.0 / 3.0).abs() < 1e-9);
        assert!((word_error_rate("um dois tres", "um quatro tres") - 1.0 / 3.0).abs() < 1e-9);
        assert!((word_error_rate("um dois", "um dois tres quatro") - 1.0).abs() < 1e-9);
    }

    #[test]
    fn empty_reference_with_output_is_a_total_error_not_a_division_by_zero() {
        assert_eq!(word_error_rate("", ""), 0.0);
        assert_eq!(word_error_rate("", "alucinou"), 1.0);
    }

    #[test]
    fn vocabulary_coverage_matches_whole_words_and_multiword_terms() {
        let (hits, misses) = vocabulary_coverage(
            &[
                "RabbitMQ".to_string(),
                "Entity Framework".to_string(),
                "Kubernetes".to_string(),
            ],
            "usamos rabbitmq com entity framework no projeto",
        );
        assert_eq!(hits, vec!["RabbitMQ", "Entity Framework"]);
        assert_eq!(misses, vec!["Kubernetes"]);
    }

    #[test]
    fn unknown_cost_is_reported_as_none_never_as_zero() {
        assert_eq!(CostModel::default().estimate(60_000), None);
        assert_eq!(CostModel::FREE_LOCAL.estimate(60_000), Some(0.0));
        let paid = CostModel {
            usd_per_audio_minute: Some(0.006),
        };
        assert!((paid.estimate(120_000).unwrap() - 0.012).abs() < 1e-9);
    }

    #[tokio::test]
    async fn runs_a_fixture_end_to_end_against_a_controlled_provider() {
        let audio = temp_wav(500);
        let provider = FakeTranscriptionProvider::new(FakeBehavior::EmitsFinal {
            text: "usamos micro serviços com rabbit mq".into(),
            partials: true,
        });
        let normalizer = DeterministicNormalizer::new(TranscriptionVocabulary::default());

        let result = run_fixture(
            &provider,
            &TranscriptionSettings::default(),
            &normalizer,
            &fixture(
                "usamos microserviços com RabbitMQ",
                &["microserviços", "RabbitMQ"],
            ),
            &audio,
            CostModel::FREE_LOCAL,
        )
        .await
        .unwrap();

        assert!(result.final_transcript_count > 0, "recebeu finais");
        assert!(result.partial_transcript_count > 0, "recebeu parciais");
        assert!(
            result.utterance_count > 0,
            "montou pelo menos uma utterance"
        );
        assert_eq!(result.latencies.audio_duration_ms, 500);
        assert_eq!(result.estimated_cost_usd, Some(0.0));
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert!(
            result.normalization_change_count > 0,
            "a normalização precisa ter mexido em 'micro serviços'/'rabbit mq'"
        );
        assert_eq!(
            result.vocabulary_misses,
            Vec::<String>::new(),
            "os dois termos técnicos sobreviveram ao pipeline: {}",
            result.normalized_transcript
        );
        assert!(
            !result.raw_transcript.is_empty()
                && result.raw_transcript != result.normalized_transcript,
            "o texto bruto é preservado ao lado do normalizado"
        );

        std::fs::remove_file(&audio).ok();
    }

    #[tokio::test]
    async fn provider_failures_are_reported_instead_of_aborting_the_case() {
        let audio = temp_wav(200);
        let provider = FakeTranscriptionProvider::new(FakeBehavior::Fails {
            message: "inferência falhou".into(),
        });
        let normalizer = DeterministicNormalizer::new(TranscriptionVocabulary::default());

        let result = run_fixture(
            &provider,
            &TranscriptionSettings::default(),
            &normalizer,
            &fixture("qualquer coisa", &[]),
            &audio,
            CostModel::FREE_LOCAL,
        )
        .await
        .unwrap();

        assert!(!result.errors.is_empty(), "o erro aparece no relatório");
        assert_eq!(result.final_transcript_count, 0);
        assert_eq!(result.word_error_rate, 1.0, "nada transcrito = erro total");
        std::fs::remove_file(&audio).ok();
    }
}
