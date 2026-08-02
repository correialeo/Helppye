//! Telemetria de pipeline: a linha do tempo de uma **fala**, do primeiro chunk de áudio até
//! o primeiro token visível da sugestão, medida ponta a ponta.
//!
//! Existe porque as latências que o app já media eram parciais e viviam em lugares
//! diferentes: `GenerationDiagnostics` conhece o trecho "utterance finalizada → token
//! visível" mas não sabe quanto tempo a transcrição levou; a fila de transcrição conhece o
//! tempo de inferência mas não sabe o que aconteceu depois. Um trace único, correlacionado
//! pelos ids que já existem (`SegmentId` → `UtteranceId` → `GenerationId`), permite
//! responder "onde foram os 4 segundos" sem adivinhação.
//!
//! Duas decisões deliberadas:
//!
//! - **Relógio monotônico.** Todo marco é um `Instant`. Epoch é sujeito a ajuste de relógio
//!   do sistema e produziria durações negativas ou absurdas exatamente durante uma reunião
//!   longa, que é quando a medição importa.
//! - **Conteúdo não é telemetria.** Por padrão nenhum texto transcrito entra num trace —
//!   só tamanhos e contagens. Texto sanitizado só aparece com `ContentPolicy::Developer`,
//!   que corresponde ao "Modo de desenvolvedor" em Configurações.

pub mod recorder;

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::audio::types::AudioSource;
use crate::conversation::SessionId;

pub use recorder::{TelemetryRecorder, TraceId};

static RECORDER: OnceLock<Arc<TelemetryRecorder>> = OnceLock::new();

/// Recorder do processo. É um singleton por uma razão concreta, não por conveniência: os
/// marcos de um mesmo trace são gravados por três subsistemas construídos em pontos
/// diferentes do `setup()` (runtime de transcrição, timeline, motor de resposta) e que não
/// se conhecem. Injetar o recorder em cada um obrigaria os três a compartilhar uma
/// dependência que só existe para observabilidade — acoplamento pior que o singleton.
/// Testes nunca usam esta função: constroem `TelemetryRecorder::new()` isolado.
pub fn recorder() -> &'static Arc<TelemetryRecorder> {
    RECORDER.get_or_init(|| Arc::new(TelemetryRecorder::new()))
}

/// Quantos traces o painel de desenvolvedor pede por vez. Um teto existe porque o comando é
/// serializado inteiro para o frontend.
const MAX_REPORTED_TRACES: usize = 50;

#[derive(Debug, Clone, Serialize)]
pub struct TelemetrySnapshot {
    pub content_policy: ContentPolicy,
    pub live_traces: usize,
    pub completed_traces: usize,
    pub recent: Vec<PipelineTraceSnapshot>,
}

/// Últimos traces concluídos. Só faz sentido atrás de "Modo de desenvolvedor" — é a visão de
/// onde o tempo foi gasto, não uma informação de uso normal.
#[tauri::command]
pub async fn telemetry_snapshot_command(limit: Option<usize>) -> Result<TelemetrySnapshot, String> {
    let recorder = recorder();
    let limit = limit
        .unwrap_or(MAX_REPORTED_TRACES)
        .min(MAX_REPORTED_TRACES);
    Ok(TelemetrySnapshot {
        content_policy: recorder.content_policy(),
        live_traces: recorder.live_count(),
        completed_traces: recorder.completed_count(),
        recent: recorder.recent(limit),
    })
}

/// Liga/desliga a retenção de texto sanitizado nos traces. Fora do modo de desenvolvedor a
/// política é `Redacted` e nenhum conteúdo de reunião é retido — este comando é a única forma
/// de mudar isso, e a mudança vale só para traces criados dali em diante.
#[tauri::command]
pub async fn telemetry_set_content_policy_command(policy: ContentPolicy) -> Result<(), String> {
    recorder().set_content_policy(policy);
    Ok(())
}

/// Marcos observáveis de uma fala, na ordem em que ocorrem. `Milestone` é um índice denso
/// justamente para que um trace seja um array de `Option<Instant>` — sem alocação por marco
/// e sem custo mensurável no caminho crítico.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Milestone {
    SpeechStartDetected,
    SpeechEndDetected,
    FirstAudioChunkSent,
    ActivityStartSent,
    LastAudioChunkSent,
    ActivityEndSent,
    FirstInputTranscriptionReceived,
    LastInputTranscriptionReceived,
    ServerTurnCompleteReceived,
    LocalFinalTranscriptEmitted,
    SpeechStarted,
    SpeechEnded,
    FirstAudioChunk,
    LastAudioChunk,
    FirstPartialTranscript,
    FinalTranscript,
    NormalizationCompleted,
    UtteranceFinalized,
    GenerationStarted,
    FirstHttpChunk,
    FirstVisibleToken,
    GenerationCompleted,
}

impl Milestone {
    pub const ALL: [Milestone; 22] = [
        Milestone::SpeechStartDetected,
        Milestone::SpeechEndDetected,
        Milestone::FirstAudioChunkSent,
        Milestone::ActivityStartSent,
        Milestone::LastAudioChunkSent,
        Milestone::ActivityEndSent,
        Milestone::FirstInputTranscriptionReceived,
        Milestone::LastInputTranscriptionReceived,
        Milestone::ServerTurnCompleteReceived,
        Milestone::LocalFinalTranscriptEmitted,
        Milestone::SpeechStarted,
        Milestone::SpeechEnded,
        Milestone::FirstAudioChunk,
        Milestone::LastAudioChunk,
        Milestone::FirstPartialTranscript,
        Milestone::FinalTranscript,
        Milestone::NormalizationCompleted,
        Milestone::UtteranceFinalized,
        Milestone::GenerationStarted,
        Milestone::FirstHttpChunk,
        Milestone::FirstVisibleToken,
        Milestone::GenerationCompleted,
    ];

    const fn index(self) -> usize {
        match self {
            Milestone::SpeechStartDetected => 0,
            Milestone::SpeechEndDetected => 1,
            Milestone::FirstAudioChunkSent => 2,
            Milestone::ActivityStartSent => 3,
            Milestone::LastAudioChunkSent => 4,
            Milestone::ActivityEndSent => 5,
            Milestone::FirstInputTranscriptionReceived => 6,
            Milestone::LastInputTranscriptionReceived => 7,
            Milestone::ServerTurnCompleteReceived => 8,
            Milestone::LocalFinalTranscriptEmitted => 9,
            Milestone::SpeechStarted => 10,
            Milestone::SpeechEnded => 11,
            Milestone::FirstAudioChunk => 12,
            Milestone::LastAudioChunk => 13,
            Milestone::FirstPartialTranscript => 14,
            Milestone::FinalTranscript => 15,
            Milestone::NormalizationCompleted => 16,
            Milestone::UtteranceFinalized => 17,
            Milestone::GenerationStarted => 18,
            Milestone::FirstHttpChunk => 19,
            Milestone::FirstVisibleToken => 20,
            Milestone::GenerationCompleted => 21,
        }
    }

    /// Marcos "último X" são sobrescritos a cada ocorrência; todos os demais são
    /// primeiro-escrito-vence, para que um segundo chunk não mova o marco do primeiro.
    const fn overwrites(self) -> bool {
        matches!(
            self,
            Milestone::LastAudioChunk
                | Milestone::LastAudioChunkSent
                | Milestone::LastInputTranscriptionReceived
        )
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Milestone::SpeechStartDetected => "speech_start_detected",
            Milestone::SpeechEndDetected => "speech_end_detected",
            Milestone::FirstAudioChunkSent => "first_audio_chunk_sent",
            Milestone::ActivityStartSent => "activity_start_sent",
            Milestone::LastAudioChunkSent => "last_audio_chunk_sent",
            Milestone::ActivityEndSent => "activity_end_sent",
            Milestone::FirstInputTranscriptionReceived => "first_input_transcription_received",
            Milestone::LastInputTranscriptionReceived => "last_input_transcription_received",
            Milestone::ServerTurnCompleteReceived => "server_turn_complete_received",
            Milestone::LocalFinalTranscriptEmitted => "local_final_transcript_emitted",
            Milestone::SpeechStarted => "speech_started",
            Milestone::SpeechEnded => "speech_ended",
            Milestone::FirstAudioChunk => "first_audio_chunk",
            Milestone::LastAudioChunk => "last_audio_chunk",
            Milestone::FirstPartialTranscript => "first_partial_transcript",
            Milestone::FinalTranscript => "final_transcript",
            Milestone::NormalizationCompleted => "normalization_completed",
            Milestone::UtteranceFinalized => "utterance_finalized",
            Milestone::GenerationStarted => "generation_started",
            Milestone::FirstHttpChunk => "first_http_chunk",
            Milestone::FirstVisibleToken => "first_visible_token",
            Milestone::GenerationCompleted => "generation_completed",
        }
    }
}

/// O que pode ser gravado junto do trace. `Redacted` é o padrão de produção: nenhum texto
/// de reunião entra em telemetria. `Developer` corresponde ao "Modo de desenvolvedor" e
/// ainda assim só grava texto **sanitizado** (colapsado e truncado), nunca o transcrito
/// inteiro.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentPolicy {
    #[default]
    Redacted,
    Developer,
}

/// Quantos caracteres de texto sanitizado no máximo, em modo de desenvolvedor.
pub const SANITIZED_PREVIEW_CHARACTERS: usize = 160;

/// Provider-internal observations that do not belong in `TranscriptionEvent` and therefore
/// cannot accidentally enter normalization or the definitive conversation timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderTelemetryEvent {
    Configuration {
        automatic_vad_enabled: bool,
        finalization_strategy: String,
    },
    AudioChunkSent {
        duration_ms: u64,
        bytes: u64,
        send_duration_ms: u64,
    },
    ActivityStartSent,
    ActivityEndSent,
    InputTranscriptionReceived,
    ServerTurnCompleteReceived,
    LocalFinalTranscriptEmitted {
        finalization_reason: String,
    },
}

impl ContentPolicy {
    /// `None` sob `Redacted`. Sob `Developer`, colapsa espaços e trunca — o objetivo é
    /// reconhecer *qual* fala é, não reconstruir a reunião a partir do log.
    pub fn sanitize(self, text: &str) -> Option<String> {
        match self {
            ContentPolicy::Redacted => None,
            ContentPolicy::Developer => {
                let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
                let truncated: String = collapsed
                    .chars()
                    .take(SANITIZED_PREVIEW_CHARACTERS)
                    .collect();
                Some(if truncated.chars().count() < collapsed.chars().count() {
                    format!("{truncated}…")
                } else {
                    truncated
                })
            }
        }
    }
}

/// Atributos não temporais de um trace — quem produziu o quê, e de que tamanho. Nenhum
/// deles é conteúdo.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct TraceAttributes {
    /// Tempo entre aceitação na fila e início do consumo pelo runtime. Se alto, o
    /// gargalo é backpressure/throughput antes do provider, não a inferência em si.
    pub transcription_queue_wait_ms: Option<u64>,
    pub provider_queue_wait_ms: Option<u64>,
    pub provider_queue_depth: Option<usize>,
    pub provider_queue_oldest_age_ms: Option<u64>,
    pub dropped_audio_chunks: Option<u64>,
    pub audio_chunk_duration_ms: Option<u64>,
    pub audio_chunks_sent: Option<u64>,
    pub bytes_sent: Option<u64>,
    pub websocket_send_latency_ms: Option<u64>,
    pub automatic_vad_enabled: Option<bool>,
    pub finalization_strategy: Option<String>,
    pub finalization_reason: Option<String>,
    pub partial_revision_count: Option<u64>,
    pub transcription_provider: Option<String>,
    pub transcription_model: Option<String>,
    pub response_provider: Option<String>,
    pub response_model: Option<String>,
    pub raw_text_length: Option<usize>,
    pub normalized_text_length: Option<usize>,
    pub normalization_change_count: Option<usize>,
    pub context_turn_count: Option<usize>,
    pub context_character_count: Option<usize>,
    /// Só preenchido sob `ContentPolicy::Developer`.
    pub sanitized_text: Option<String>,
}

/// Trace vivo. Os marcos são deslocamentos a partir de `origin`, um único `Instant` capturado
/// na abertura: guardar `Duration` em vez de `Instant` mantém o snapshot serializável sem
/// nenhuma conversão para epoch.
#[derive(Debug, Clone)]
pub struct PipelineTrace {
    id: TraceId,
    session_id: SessionId,
    source: AudioSource,
    origin: Instant,
    marks: [Option<Duration>; Milestone::ALL.len()],
    attributes: TraceAttributes,
}

impl PipelineTrace {
    pub(crate) fn new_at(
        id: TraceId,
        session_id: SessionId,
        source: AudioSource,
        origin: Instant,
    ) -> Self {
        PipelineTrace {
            id,
            session_id,
            source,
            origin,
            marks: Default::default(),
            attributes: TraceAttributes::default(),
        }
    }

    pub fn id(&self) -> TraceId {
        self.id
    }

    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub fn source(&self) -> AudioSource {
        self.source
    }

    pub(crate) fn mark(&mut self, milestone: Milestone) {
        self.mark_at(milestone, Instant::now());
    }

    /// Grava um marco que ocorreu num instante já passado. Necessário porque o motor de
    /// resposta mede seus próprios `Instant` dentro do laço de streaming (onde não pode
    /// pegar o lock do recorder a cada chunk) e só os reporta ao terminar a geração.
    pub(crate) fn mark_at(&mut self, milestone: Milestone, at: Instant) {
        let slot = &mut self.marks[milestone.index()];
        if slot.is_none() || milestone.overwrites() {
            *slot = Some(at.saturating_duration_since(self.origin));
        }
    }

    pub fn at(&self, milestone: Milestone) -> Option<Duration> {
        self.marks[milestone.index()]
    }

    pub(crate) fn attributes_mut(&mut self) -> &mut TraceAttributes {
        &mut self.attributes
    }

    pub fn attributes(&self) -> &TraceAttributes {
        &self.attributes
    }

    /// Duração entre dois marcos. `None` quando qualquer um dos dois não ocorreu — nunca
    /// zero, que seria indistinguível de "aconteceu instantaneamente".
    pub fn between(&self, from: Milestone, to: Milestone) -> Option<Duration> {
        let (from, to) = (self.at(from)?, self.at(to)?);
        to.checked_sub(from)
    }

    pub fn snapshot(&self) -> PipelineTraceSnapshot {
        let marks = Milestone::ALL
            .iter()
            .filter_map(|m| self.at(*m).map(|d| (m.as_str().to_string(), millis(d))))
            .collect();
        PipelineTraceSnapshot {
            trace_id: self.id,
            session_id: self.session_id,
            source: self.source,
            marks,
            latencies: PipelineLatencies::from_trace(self),
            attributes: self.attributes.clone(),
        }
    }
}

/// As cinco durações derivadas exigidas pelo pipeline, todas em milissegundos e todas
/// opcionais: uma fala que o modelo decidiu não responder simplesmente não tem
/// `generation_started → first_visible_token`, e reportar `0` ali seria mentira.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct PipelineLatencies {
    pub speech_start_to_first_partial_ms: Option<u64>,
    pub speech_end_to_activity_end_ms: Option<u64>,
    pub activity_end_to_last_partial_ms: Option<i64>,
    pub activity_end_to_final_transcript_ms: Option<u64>,
    pub speech_end_to_final_transcript_ms: Option<u64>,
    pub speech_ended_to_final_transcript_ms: Option<u64>,
    pub final_transcript_to_utterance_finalized_ms: Option<u64>,
    pub utterance_finalized_to_generation_started_ms: Option<u64>,
    pub generation_started_to_first_visible_token_ms: Option<u64>,
    pub speech_ended_to_first_visible_token_ms: Option<u64>,
}

impl PipelineLatencies {
    pub fn from_trace(trace: &PipelineTrace) -> Self {
        let span = |from, to| trace.between(from, to).map(millis);
        let signed_span = |from, to| {
            let from = trace.at(from)?;
            let to = trace.at(to)?;
            if to >= from {
                Some(i64::try_from((to - from).as_millis()).unwrap_or(i64::MAX))
            } else {
                Some(-i64::try_from((from - to).as_millis()).unwrap_or(i64::MAX))
            }
        };
        PipelineLatencies {
            speech_start_to_first_partial_ms: span(
                Milestone::SpeechStartDetected,
                Milestone::FirstInputTranscriptionReceived,
            ),
            speech_end_to_activity_end_ms: span(
                Milestone::SpeechEndDetected,
                Milestone::ActivityEndSent,
            ),
            activity_end_to_last_partial_ms: signed_span(
                Milestone::ActivityEndSent,
                Milestone::LastInputTranscriptionReceived,
            ),
            activity_end_to_final_transcript_ms: span(
                Milestone::ActivityEndSent,
                Milestone::LocalFinalTranscriptEmitted,
            ),
            speech_end_to_final_transcript_ms: span(
                Milestone::SpeechEndDetected,
                Milestone::LocalFinalTranscriptEmitted,
            ),
            speech_ended_to_final_transcript_ms: span(
                Milestone::SpeechEnded,
                Milestone::FinalTranscript,
            ),
            final_transcript_to_utterance_finalized_ms: span(
                Milestone::FinalTranscript,
                Milestone::UtteranceFinalized,
            ),
            utterance_finalized_to_generation_started_ms: span(
                Milestone::UtteranceFinalized,
                Milestone::GenerationStarted,
            ),
            generation_started_to_first_visible_token_ms: span(
                Milestone::GenerationStarted,
                Milestone::FirstVisibleToken,
            ),
            speech_ended_to_first_visible_token_ms: span(
                Milestone::SpeechEnded,
                Milestone::FirstVisibleToken,
            ),
        }
    }
}

/// Forma serializável de um trace, entregue ao modo de desenvolvedor e ao harness de
/// benchmark.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PipelineTraceSnapshot {
    pub trace_id: TraceId,
    pub session_id: SessionId,
    pub source: AudioSource,
    /// Deslocamento em ms desde a abertura do trace, por marco ocorrido.
    pub marks: Vec<(String, u64)>,
    pub latencies: PipelineLatencies,
    pub attributes: TraceAttributes,
}

fn millis(duration: Duration) -> u64 {
    // `as_millis` devolve u128; saturar em u64 é seguro aqui (u64::MAX ms ≈ 584 milhões de
    // anos) e evita um `unwrap` em caminho de telemetria.
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trace() -> PipelineTrace {
        PipelineTrace::new_at(
            TraceId(1),
            SessionId::from_value(1),
            AudioSource::SystemOutput,
            Instant::now(),
        )
    }

    #[test]
    fn first_write_wins_except_for_last_audio_chunk() {
        let mut t = trace();
        t.mark(Milestone::FirstAudioChunk);
        let first = t.at(Milestone::FirstAudioChunk).unwrap();
        std::thread::sleep(Duration::from_millis(2));
        t.mark(Milestone::FirstAudioChunk);
        assert_eq!(t.at(Milestone::FirstAudioChunk), Some(first));

        t.mark(Milestone::LastAudioChunk);
        let last = t.at(Milestone::LastAudioChunk).unwrap();
        std::thread::sleep(Duration::from_millis(2));
        t.mark(Milestone::LastAudioChunk);
        assert!(t.at(Milestone::LastAudioChunk).unwrap() > last);
    }

    #[test]
    fn missing_milestone_yields_none_not_zero() {
        let mut t = trace();
        t.mark(Milestone::SpeechEnded);
        let latencies = PipelineLatencies::from_trace(&t);
        assert_eq!(latencies.speech_ended_to_final_transcript_ms, None);
        assert_eq!(latencies.speech_ended_to_first_visible_token_ms, None);
    }

    #[test]
    fn derived_latencies_are_computed_when_both_marks_exist() {
        let mut t = trace();
        t.mark(Milestone::SpeechEnded);
        std::thread::sleep(Duration::from_millis(5));
        t.mark(Milestone::FinalTranscript);
        t.mark(Milestone::UtteranceFinalized);
        t.mark(Milestone::GenerationStarted);
        std::thread::sleep(Duration::from_millis(5));
        t.mark(Milestone::FirstVisibleToken);

        let l = PipelineLatencies::from_trace(&t);
        assert!(l.speech_ended_to_final_transcript_ms.unwrap() >= 5);
        assert!(l.generation_started_to_first_visible_token_ms.unwrap() >= 5);
        assert!(l.speech_ended_to_first_visible_token_ms.unwrap() >= 10);
        assert!(l.final_transcript_to_utterance_finalized_ms.is_some());
    }

    #[test]
    fn redacted_policy_never_returns_content() {
        assert_eq!(ContentPolicy::Redacted.sanitize("uma fala qualquer"), None);
    }

    #[test]
    fn developer_policy_collapses_and_truncates() {
        let long = "a ".repeat(300);
        let sanitized = ContentPolicy::Developer.sanitize(&long).unwrap();
        assert!(sanitized.ends_with('…'));
        assert_eq!(
            sanitized.chars().count(),
            SANITIZED_PREVIEW_CHARACTERS + 1,
            "trunca no limite, mais o marcador de truncamento"
        );

        assert_eq!(
            ContentPolicy::Developer.sanitize("  duas   palavras  "),
            Some("duas palavras".to_string())
        );
    }

    #[test]
    fn snapshot_only_lists_milestones_that_happened() {
        let mut t = trace();
        t.mark(Milestone::FirstAudioChunk);
        t.mark(Milestone::FinalTranscript);
        let snapshot = t.snapshot();
        let names: Vec<&str> = snapshot.marks.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["first_audio_chunk", "final_transcript"]);
    }
}
