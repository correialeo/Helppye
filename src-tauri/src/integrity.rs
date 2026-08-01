//! Integridade causal de origem.
//!
//! Uma fala capturada pela saída de sistema é da outra pessoa; uma capturada pelo microfone
//! é do usuário. Essa correspondência é a única coisa que separa "sugerir uma resposta" de
//! "sugerir uma resposta para a própria fala do usuário", e ela não pode depender de
//! conteúdo, de ordem de chegada nem da última fonte ativa.
//!
//! Este módulo guarda três coisas, e vale distinguir bem o papel de cada uma:
//!
//! 1. **`SourceIntegrityError`** — a origem *mudou* dentro do processo entre dois estágios do
//!    pipeline. Isso é sempre um defeito de software, nunca um fenômeno físico. O tratamento
//!    é rejeitar o dado, não consertá-lo: um segmento cuja origem já divergiu não tem uma
//!    "origem certa" recuperável a posteriori.
//! 2. **`CrossSourceDiagnosis`** — dois segmentos *diferentes*, um por fonte, com texto e
//!    janela de tempo parecidos. Isso é físico (o alto-falante toca, o microfone escuta) e
//!    **não** é uma troca de origem: cada segmento tem a fonte real dele. O diagnóstico
//!    existe para explicar o fenômeno, nunca para reescrever `speaker`/`source`.
//! 3. **`OriginIntegrityLog`** — o rastro por resultado que torna as duas coisas acima
//!    verificáveis em modo de desenvolvedor sem despejar o conteúdo da conversa em log.
//!
//! Nada aqui infere origem a partir de texto. Texto só é usado para *comparar* dois
//! segmentos que já têm origem própria, e sempre em forma de hash quando vai para log.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};

use serde::Serialize;
use thiserror::Error;

use crate::audio::segment::SegmentId;
use crate::audio::types::AudioSource;

/// Estágio do pipeline em que a divergência foi observada. O valor não é decorativo: é ele
/// que diz se o defeito está antes ou depois da fila de transcrição, que é a diferença
/// entre "o provider mentiu a fonte" e "a timeline atribuiu errado".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrityStage {
    /// Entre a produção do segmento e a entrada na fila de transcrição.
    Enqueue,
    /// Entre o envelope enviado ao provider e o resultado devolvido por ele.
    TranscriptionResult,
    /// Entre o resultado da transcrição e o `TranscriptSegment` criado pela timeline.
    Timeline,
    /// Entre o `TranscriptSegment` e a utterance/turno em que ele seria agrupado.
    TimelineAssembly,
    /// Entre a utterance elegível e a `ResponseGenerationRequest` montada a partir dela.
    ResponseTrigger,
}

impl IntegrityStage {
    pub fn as_str(self) -> &'static str {
        match self {
            IntegrityStage::Enqueue => "enqueue",
            IntegrityStage::TranscriptionResult => "transcription_result",
            IntegrityStage::Timeline => "timeline",
            IntegrityStage::TimelineAssembly => "timeline_assembly",
            IntegrityStage::ResponseTrigger => "response_trigger",
        }
    }
}

/// A origem de um segmento divergiu entre dois estágios. Erro tipado (não uma string) porque
/// é consumido por decisão de fluxo — rejeitar o evento — e não só por log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error, Serialize)]
#[error(
    "integridade de origem violada em {stage:?}: segmento {segment_id:?} foi capturado como \
     {expected_source:?} mas chegou como {observed_source:?}"
)]
pub struct SourceIntegrityError {
    pub segment_id: SegmentId,
    pub expected_source: AudioSource,
    pub observed_source: AudioSource,
    pub stage: IntegrityStage,
}

impl SourceIntegrityError {
    pub fn new(
        segment_id: SegmentId,
        expected_source: AudioSource,
        observed_source: AudioSource,
        stage: IntegrityStage,
    ) -> Self {
        SourceIntegrityError {
            segment_id,
            expected_source,
            observed_source,
            stage,
        }
    }

    /// Compara o que foi capturado com o que chegou. `Ok(())` quando batem — o caso normal,
    /// e por isso barato.
    pub fn check(
        segment_id: SegmentId,
        expected_source: AudioSource,
        observed_source: AudioSource,
        stage: IntegrityStage,
    ) -> Result<(), SourceIntegrityError> {
        if expected_source == observed_source {
            Ok(())
        } else {
            Err(SourceIntegrityError::new(
                segment_id,
                expected_source,
                observed_source,
                stage,
            ))
        }
    }
}

/// Como um segmento entrou na timeline. Só `Live` é fala real; as demais variantes existem
/// para que um segmento suspeito continue **visível em diagnóstico** sem participar da
/// conversa — apagá-lo tornaria o fenômeno indiagnosticável.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptSegmentOrigin {
    #[default]
    Live,
    /// Segmento de microfone que, com alta confiança, é o alto-falante sendo re-capturado —
    /// não o usuário falando. Ver `CrossSourceDiagnosis::ProbableAcousticLeak`.
    ProbableSystemAudioLeak,
}

/// Classificação de dois segmentos de **fontes diferentes** que se parecem.
///
/// A distinção que importa é a primeira: `InternalSourceMismatch` é um bug do processo (o
/// *mesmo* `segment_id` aparecendo com duas fontes), enquanto as outras três descrevem dois
/// segmentos genuinamente distintos. Tratar as duas coisas como "duplicata" foi o que
/// tornaria o defeito real invisível atrás de uma supressão.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CrossSourceDiagnosis {
    /// O mesmo segmento foi visto com duas fontes. Nunca é acústico: é roteamento interno.
    InternalSourceMismatch,
    /// Dois segmentos distintos, um por fonte, com texto quase idêntico e sobreposição
    /// temporal compatível com o áudio da outra pessoa vazando para o microfone.
    ProbableAcousticLeak,
    /// Dois segmentos distintos da **mesma** fonte com texto quase idêntico e janelas
    /// sobrepostas — captura entregue duas vezes, não duas falas.
    ProbableDuplicateCapture,
    /// Falas independentes. Inclui o caso de as duas pessoas falarem ao mesmo tempo e o de a
    /// mesma frase ser dita em momentos diferentes.
    IndependentSpeech,
}

impl CrossSourceDiagnosis {
    pub fn as_str(self) -> &'static str {
        match self {
            CrossSourceDiagnosis::InternalSourceMismatch => "internal_source_mismatch",
            CrossSourceDiagnosis::ProbableAcousticLeak => "probable_acoustic_leak",
            CrossSourceDiagnosis::ProbableDuplicateCapture => "probable_duplicate_capture",
            CrossSourceDiagnosis::IndependentSpeech => "independent_speech",
        }
    }
}

/// O que se sabe de um segmento para efeito de comparação cruzada. Deliberadamente pequeno:
/// identidade, janela e texto normalizado. Nada de áudio, nada de provider.
#[derive(Debug, Clone, PartialEq)]
pub struct CrossSourceCandidate {
    pub segment_id: SegmentId,
    pub source: AudioSource,
    pub started_at_ms: u64,
    pub ended_at_ms: u64,
    pub text: String,
}

#[derive(Debug, Clone, Copy)]
pub struct CrossSourceConfig {
    /// Similaridade de texto (0.0–1.0) a partir da qual dois segmentos são considerados a
    /// mesma fala. Alto de propósito: abaixo disso preferimos chamar de fala independente.
    pub similarity_threshold: f32,
    /// Deslocamento máximo entre os inícios para que a sobreposição temporal seja plausível
    /// como eco acústico. Eco é quase instantâneo; a folga cobre a granularidade do VAD.
    pub maximum_start_skew_ms: u64,
}

impl Default for CrossSourceConfig {
    fn default() -> Self {
        CrossSourceConfig {
            similarity_threshold: 0.88,
            maximum_start_skew_ms: 1_200,
        }
    }
}

/// Diagnostica — e **só** diagnostica. Nenhum chamador desta função altera `speaker` ou
/// `source` a partir do resultado; a fonte física de cada segmento é preservada como está.
pub fn diagnose_cross_source(
    a: &CrossSourceCandidate,
    b: &CrossSourceCandidate,
    config: CrossSourceConfig,
) -> CrossSourceDiagnosis {
    // Mesmo id com fontes diferentes é, por definição, o processo se contradizendo: nenhuma
    // quantidade de similaridade de texto muda esse veredito, então ele vem antes de tudo.
    if a.segment_id == b.segment_id {
        if a.source != b.source {
            return CrossSourceDiagnosis::InternalSourceMismatch;
        }
        return CrossSourceDiagnosis::ProbableDuplicateCapture;
    }

    let similarity = text_similarity(&a.text, &b.text);
    if similarity < config.similarity_threshold {
        return CrossSourceDiagnosis::IndependentSpeech;
    }

    let start_skew = a.started_at_ms.abs_diff(b.started_at_ms);
    let overlaps = a.started_at_ms < b.ended_at_ms && b.started_at_ms < a.ended_at_ms;
    if start_skew > config.maximum_start_skew_ms || !overlaps {
        // Mesma frase em momentos diferentes é repetição legítima ("Você pode repetir?" /
        // "Você pode repetir?"), não eco. Suprimir isso apagaria fala real.
        return CrossSourceDiagnosis::IndependentSpeech;
    }

    if a.source == b.source {
        CrossSourceDiagnosis::ProbableDuplicateCapture
    } else {
        CrossSourceDiagnosis::ProbableAcousticLeak
    }
}

/// Similaridade por bag-of-words normalizado (Jaccard sobre tokens minúsculos e sem
/// pontuação). Escolhida por ser barata e simétrica: roda no caminho crítico entre a
/// transcrição e a timeline, onde qualquer custo é somado à métrica de latência que o
/// produto mede.
pub fn text_similarity(a: &str, b: &str) -> f32 {
    let tokens_a = tokenize(a);
    let tokens_b = tokenize(b);
    if tokens_a.is_empty() && tokens_b.is_empty() {
        return 1.0;
    }
    if tokens_a.is_empty() || tokens_b.is_empty() {
        return 0.0;
    }
    let mut intersection = 0usize;
    let mut remaining = tokens_b.clone();
    for token in &tokens_a {
        if let Some(position) = remaining.iter().position(|candidate| candidate == token) {
            remaining.remove(position);
            intersection += 1;
        }
    }
    let union = tokens_a.len() + tokens_b.len() - intersection;
    intersection as f32 / union as f32
}

fn tokenize(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|word| {
            word.chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(|c| c.to_lowercase())
                .collect::<String>()
        })
        .filter(|word| !word.is_empty())
        .collect()
}

/// Hash estável-por-execução de um texto. É o que vai para log e para o painel de
/// diagnóstico no lugar do conteúdo: permite comparar "é o mesmo texto?" entre estágios sem
/// que a conversa do usuário apareça em nenhum log de produção.
pub fn text_hash(text: &str) -> String {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrityStatus {
    Ok,
    /// A origem divergiu e o dado foi rejeitado.
    SourceMismatch,
    /// Identidade do segmento não pôde ser resolvida e caiu no fallback FIFO do próprio
    /// fluxo. Não é erro, mas é informação: o resultado é atribuído por ordem, não por id.
    ResolvedByFifoFallback,
}

/// Rastro de um resultado de transcrição atravessando o pipeline, com a origem observada em
/// cada estágio. É o instrumento que torna a afirmação "a fonte permanece `SystemOutput` da
/// captura até a geração" verificável em vez de declarada.
#[derive(Debug, Clone, Serialize)]
pub struct OriginObservation {
    pub session_id: u64,
    pub capture_stream_id: u64,
    pub segment_id: SegmentId,
    pub sequence_number: u64,
    pub source_at_capture: AudioSource,
    pub source_at_queue: AudioSource,
    pub source_at_transcription_result: AudioSource,
    pub source_at_timeline: Option<AudioSource>,
    pub derived_speaker: Option<&'static str>,
    pub audio_started_at_ms: u64,
    pub audio_ended_at_ms: u64,
    pub transcription_completed_at_ms: u64,
    pub raw_text_hash: String,
    pub normalized_text_hash: String,
    pub cross_source_similarity: Option<f32>,
    pub integrity_status: IntegrityStatus,
}

/// Quantas observações ficam disponíveis. Limitado pelo mesmo motivo que a telemetria limita
/// traces: uma reunião longa produziria um vetor sem fim, e o valor de diagnóstico está nas
/// últimas falas, não nas primeiras.
pub const MAX_ORIGIN_OBSERVATIONS: usize = 256;

#[derive(Default)]
pub struct OriginIntegrityLog {
    observations: Mutex<Vec<OriginObservation>>,
    violations: Mutex<Vec<SourceIntegrityError>>,
}

impl OriginIntegrityLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, observation: OriginObservation) {
        let mut observations = self.observations.lock().expect("origin integrity mutex");
        if observations.len() >= MAX_ORIGIN_OBSERVATIONS {
            observations.remove(0);
        }
        observations.push(observation);
    }

    /// Completa a observação já registrada no estágio de transcrição com o que só a timeline
    /// sabe. É atualização, não uma segunda entrada: o valor do rastro está em ver **um**
    /// segmento com a origem de cada estágio lado a lado — duas linhas parciais para o mesmo
    /// `segment_id` obrigariam quem lê o painel a cruzá-las de cabeça, que é exatamente o
    /// trabalho que este log existe para dispensar.
    pub fn complete_at_timeline(
        &self,
        segment_id: SegmentId,
        source_at_timeline: AudioSource,
        derived_speaker: &'static str,
        cross_source_similarity: Option<f32>,
    ) {
        let mut observations = self.observations.lock().expect("origin integrity mutex");
        if let Some(observation) = observations
            .iter_mut()
            .rev()
            .find(|observation| observation.segment_id == segment_id)
        {
            observation.source_at_timeline = Some(source_at_timeline);
            observation.derived_speaker = Some(derived_speaker);
            observation.cross_source_similarity = cross_source_similarity;
        }
    }

    pub fn record_violation(&self, error: SourceIntegrityError) {
        // `error` (não `warn`): uma troca de origem dentro do processo não é degradação, é
        // um dado que deixou de ser verdadeiro. Nunca inclui texto.
        tracing::error!(
            segment_id = ?error.segment_id,
            expected_source = ?error.expected_source,
            observed_source = ?error.observed_source,
            stage = error.stage.as_str(),
            "source_integrity_violation"
        );
        let mut violations = self.violations.lock().expect("origin integrity mutex");
        if violations.len() >= MAX_ORIGIN_OBSERVATIONS {
            violations.remove(0);
        }
        violations.push(error);
    }

    pub fn snapshot(&self) -> OriginIntegritySnapshot {
        OriginIntegritySnapshot {
            observations: self
                .observations
                .lock()
                .expect("origin integrity mutex")
                .clone(),
            violations: self
                .violations
                .lock()
                .expect("origin integrity mutex")
                .clone(),
        }
    }

    pub fn clear(&self) {
        self.observations
            .lock()
            .expect("origin integrity mutex")
            .clear();
        self.violations
            .lock()
            .expect("origin integrity mutex")
            .clear();
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct OriginIntegritySnapshot {
    pub observations: Vec<OriginObservation>,
    pub violations: Vec<SourceIntegrityError>,
}

static ORIGIN_LOG: OnceLock<Arc<OriginIntegrityLog>> = OnceLock::new();

/// Singleton de processo, pela mesma razão declarada em `docs/telemetry.md` para o recorder:
/// os estágios que observam origem são construídos em pontos diferentes do `setup()` e não
/// se conhecem; injetar o log nos três os acoplaria por algo que só existe para
/// observabilidade. **Testes nunca usam esta função** — constroem `OriginIntegrityLog::new()`.
pub fn origin_log() -> &'static Arc<OriginIntegrityLog> {
    ORIGIN_LOG.get_or_init(|| Arc::new(OriginIntegrityLog::new()))
}

/// Snapshot do rastro de origem. Exposto ao frontend apenas atrás de "Modo de
/// desenvolvedor" — ver `DeveloperToolsScreen`. Não carrega texto, só hashes.
#[tauri::command]
pub async fn origin_integrity_snapshot_command() -> Result<OriginIntegritySnapshot, String> {
    Ok(origin_log().snapshot())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(
        source: AudioSource,
        started_at_ms: u64,
        ended_at_ms: u64,
        text: &str,
    ) -> CrossSourceCandidate {
        CrossSourceCandidate {
            segment_id: SegmentId::next(),
            source,
            started_at_ms,
            ended_at_ms,
            text: text.to_string(),
        }
    }

    #[test]
    fn same_segment_id_with_two_sources_is_an_internal_mismatch_not_an_echo() {
        let mut a = candidate(AudioSource::SystemOutput, 0, 1_000, "olá");
        let mut b = a.clone();
        b.source = AudioSource::Microphone;
        a.text = "olá".into();
        b.text = "totalmente diferente".into();
        assert_eq!(
            diagnose_cross_source(&a, &b, CrossSourceConfig::default()),
            CrossSourceDiagnosis::InternalSourceMismatch
        );
    }

    #[test]
    fn near_identical_overlapping_speech_from_both_sources_is_a_probable_acoustic_leak() {
        let a = candidate(
            AudioSource::SystemOutput,
            1_000,
            3_000,
            "em qual situação você escolheria usar microserviços",
        );
        let b = candidate(
            AudioSource::Microphone,
            1_100,
            3_050,
            "em qual situação você escolheria usar microserviços",
        );
        assert_eq!(
            diagnose_cross_source(&a, &b, CrossSourceConfig::default()),
            CrossSourceDiagnosis::ProbableAcousticLeak
        );
    }

    #[test]
    fn simultaneous_but_different_speech_is_independent() {
        let a = candidate(
            AudioSource::SystemOutput,
            1_000,
            3_000,
            "me conta um caso real",
        );
        let b = candidate(AudioSource::Microphone, 1_050, 3_000, "claro, posso falar");
        assert_eq!(
            diagnose_cross_source(&a, &b, CrossSourceConfig::default()),
            CrossSourceDiagnosis::IndependentSpeech
        );
    }

    #[test]
    fn the_same_sentence_at_a_different_time_is_not_an_echo() {
        let a = candidate(AudioSource::SystemOutput, 1_000, 3_000, "você pode repetir");
        let b = candidate(AudioSource::Microphone, 60_000, 62_000, "você pode repetir");
        assert_eq!(
            diagnose_cross_source(&a, &b, CrossSourceConfig::default()),
            CrossSourceDiagnosis::IndependentSpeech
        );
    }

    #[test]
    fn moderate_similarity_is_not_enough_to_call_it_an_echo() {
        let a = candidate(
            AudioSource::SystemOutput,
            1_000,
            3_000,
            "em qual situação você escolheria usar monolitos",
        );
        let b = candidate(
            AudioSource::Microphone,
            1_050,
            3_000,
            "em geral eu evito usar isso hoje em dia sem medir antes",
        );
        assert!(
            text_similarity(&a.text, &b.text) < CrossSourceConfig::default().similarity_threshold
        );
        assert_eq!(
            diagnose_cross_source(&a, &b, CrossSourceConfig::default()),
            CrossSourceDiagnosis::IndependentSpeech
        );
    }

    #[test]
    fn integrity_check_passes_when_sources_agree_and_fails_when_they_do_not() {
        let id = SegmentId::next();
        assert!(SourceIntegrityError::check(
            id,
            AudioSource::SystemOutput,
            AudioSource::SystemOutput,
            IntegrityStage::TranscriptionResult
        )
        .is_ok());
        let error = SourceIntegrityError::check(
            id,
            AudioSource::SystemOutput,
            AudioSource::Microphone,
            IntegrityStage::TranscriptionResult,
        )
        .unwrap_err();
        assert_eq!(error.expected_source, AudioSource::SystemOutput);
        assert_eq!(error.observed_source, AudioSource::Microphone);
        assert_eq!(error.stage, IntegrityStage::TranscriptionResult);
    }

    #[test]
    fn text_hash_never_leaks_the_text_itself() {
        let hash = text_hash("Me conta um caso real em que você optou por usar monólito.");
        assert_eq!(hash.len(), 16);
        assert!(!hash.contains("monólito"));
        assert_eq!(
            hash,
            text_hash("Me conta um caso real em que você optou por usar monólito.")
        );
    }

    #[test]
    fn the_log_keeps_only_the_most_recent_observations() {
        let log = OriginIntegrityLog::new();
        for _ in 0..(MAX_ORIGIN_OBSERVATIONS + 10) {
            log.record(OriginObservation {
                session_id: 1,
                capture_stream_id: 1,
                segment_id: SegmentId::next(),
                sequence_number: 1,
                source_at_capture: AudioSource::SystemOutput,
                source_at_queue: AudioSource::SystemOutput,
                source_at_transcription_result: AudioSource::SystemOutput,
                source_at_timeline: Some(AudioSource::SystemOutput),
                derived_speaker: Some("other_person"),
                audio_started_at_ms: 0,
                audio_ended_at_ms: 100,
                transcription_completed_at_ms: 0,
                raw_text_hash: text_hash("a"),
                normalized_text_hash: text_hash("a"),
                cross_source_similarity: None,
                integrity_status: IntegrityStatus::Ok,
            });
        }
        assert_eq!(log.snapshot().observations.len(), MAX_ORIGIN_OBSERVATIONS);
    }
}
