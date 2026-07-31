//! Conversation Timeline assembly.
//!
//! `TranscriptSegment` is the raw output of local transcription.
//! `ConversationUtterance` is a short phrase/block assembled from nearby segments.
//! `ConversationTurn` is everything one speaker said while holding the floor, possibly
//! spanning several utterances. This layer is text-only: it does not correct
//! transcription, summarize, call an LLM, or discard raw segment IDs.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::audio::segment::{AudioTimestamp, SegmentId};
use crate::audio::types::{AudioCaptureEvent, AudioSource};
use crate::normalization::{NormalizationChange, TranscriptNormalizationResult};
use crate::response_provider::engine::{
    is_eligible_turn, process_conversation_events, GenerationTrigger,
};
use crate::response_provider::ResponseEngineState;
use crate::transcription::events::FinalTranscript;
#[cfg(test)]
use crate::transcription::types::{Transcript, TranscriptEvent};

pub const CONVERSATION_TIMELINE_EVENT: &str = "conversation://timeline-event";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationSpeaker {
    User,
    OtherPerson,
}

impl From<AudioSource> for ConversationSpeaker {
    fn from(source: AudioSource) -> Self {
        match source {
            AudioSource::Microphone => ConversationSpeaker::User,
            AudioSource::SystemOutput => ConversationSpeaker::OtherPerson,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct UtteranceId(u64);

impl UtteranceId {
    #[cfg(test)]
    pub(crate) fn from_raw(value: u64) -> Self {
        UtteranceId(value)
    }

    pub fn value(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct TurnId(u64);

impl TurnId {
    #[cfg(test)]
    pub(crate) fn from_raw(value: u64) -> Self {
        TurnId(value)
    }

    pub fn value(self) -> u64 {
        self.0
    }
}

/// Contador global de sessões do processo. Monotônico (não derivado do relógio de parede,
/// que pode repetir ou andar para trás): a identidade da sessão é usada para *rejeitar*
/// trabalho antigo, então dois ids iguais em sessões diferentes seriam uma falha de
/// isolamento, não um detalhe cosmético.
static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

/// Identifica uma sessão de conversa (do "Iniciar sessão" ao "Encerrar sessão"), não o
/// processo do app: encerrar uma sessão descarta todo o estado conversacional e cria uma
/// nova `SessionId`. É a fronteira rígida que o `ResponseEngine` valida antes de montar
/// contexto, iniciar geração ou publicar qualquer evento — ver
/// `docs/response-suggestion.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct SessionId(u64);

impl SessionId {
    pub(crate) fn new() -> Self {
        SessionId(NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed))
    }

    pub fn value(self) -> u64 {
        self.0
    }

    /// Só para testes: sessões com valor conhecido tornam verificável o descarte de evento
    /// de sessão anterior, que de outra forma dependeria do estado do contador global.
    #[cfg(test)]
    pub(crate) fn from_value(value: u64) -> Self {
        SessionId(value)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TranscriptSegment {
    pub segment_id: SegmentId,
    pub speaker: ConversationSpeaker,
    pub source: AudioSource,
    /// Texto **normalizado** — o que a timeline junta, o que vai para o prompt, o que o
    /// usuário vê. `raw_text` guarda o que o transcritor entregou de fato; os dois nunca
    /// aparecem juntos no contexto enviado ao modelo (ver `ResponseContextBuilder`).
    pub text: String,
    /// Texto original do provider, preservado sempre. Serve a diagnóstico e a comparação de
    /// backends no benchmark; nunca é descartado, mesmo quando a normalização não mudou
    /// nada.
    pub raw_text: String,
    /// O que a normalização mexeu, item a item. Vazio quando o texto já estava correto.
    pub normalization_changes: Vec<NormalizationChange>,
    pub language: Option<String>,
    pub started_at: AudioTimestamp,
    pub ended_at: AudioTimestamp,
    pub processing_time_ms: u64,
    pub received_sequence: u64,
}

impl TranscriptSegment {
    /// Caminho **de teste**. Em produção nenhum segmento nasce de um `Transcript` cru: ele
    /// passa antes pelo `TranscriptionRuntime`, que valida a sessão e normaliza (ver
    /// `from_normalized`). Manter este construtor restrito a testes é o que impede que uma
    /// chamada nova pule a validação de sessão sem ninguém perceber.
    #[cfg(test)]
    fn from_transcript(transcript: Transcript, received_sequence: u64) -> Option<Self> {
        let text = normalize_segment_text(&transcript.text);
        if text.is_empty() {
            return None;
        }

        Some(TranscriptSegment {
            segment_id: transcript.segment_id,
            speaker: ConversationSpeaker::from(transcript.source),
            source: transcript.source,
            raw_text: transcript.text,
            text,
            normalization_changes: Vec::new(),
            language: transcript.language,
            started_at: transcript.started_at,
            ended_at: transcript.ended_at,
            processing_time_ms: transcript.processing_time_ms,
            received_sequence,
        })
    }

    /// Segmento a partir de um resultado final já normalizado pelo
    /// `TranscriptionRuntime` — o caminho de produção. `normalize_segment_text` ainda roda
    /// por cima como rede de segurança para espaços residuais; ela é idempotente sobre
    /// texto já normalizado.
    fn from_normalized(
        transcript: &FinalTranscript,
        normalization: &TranscriptNormalizationResult,
        received_sequence: u64,
    ) -> Option<Self> {
        let text = normalize_segment_text(&normalization.normalized_text);
        if text.is_empty() {
            return None;
        }

        Some(TranscriptSegment {
            segment_id: transcript.segment_id.unwrap_or_else(SegmentId::next),
            speaker: ConversationSpeaker::from(transcript.source),
            source: transcript.source,
            raw_text: normalization.raw_text.clone(),
            text,
            normalization_changes: normalization.normalization_changes.clone(),
            language: transcript.language.clone(),
            started_at: transcript.started_at,
            ended_at: transcript.ended_at,
            processing_time_ms: transcript.processing_time_ms.unwrap_or(0),
            received_sequence,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConversationUtterance {
    pub id: UtteranceId,
    pub speaker: ConversationSpeaker,
    pub source: AudioSource,
    pub text: String,
    pub segments: Vec<SegmentId>,
    pub started_at: AudioTimestamp,
    pub ended_at: AudioTimestamp,
    pub finalized_at: Option<AudioTimestamp>,
    /// Incrementada a cada segmento anexado. Usada pelo timer dedicado da utterance
    /// (`ConversationTimeline::reschedule_utterance_timer`) para detectar, ao expirar, se
    /// a utterance ainda é a mesma que estava aberta quando o timer foi agendado — se a
    /// revisão mudou (ou a utterance já finalizou por outro caminho), o timer é um no-op.
    pub revision: u64,
}

impl ConversationUtterance {
    fn start(id: UtteranceId, segment: &TranscriptSegment) -> Self {
        ConversationUtterance {
            id,
            speaker: segment.speaker,
            source: segment.source,
            text: segment.text.clone(),
            segments: vec![segment.segment_id],
            started_at: segment.started_at,
            ended_at: segment.ended_at,
            finalized_at: None,
            revision: 1,
        }
    }

    fn append(&mut self, segment: &TranscriptSegment) {
        self.text = join_text(&self.text, &segment.text);
        self.segments.push(segment.segment_id);
        self.ended_at = std::cmp::max(self.ended_at, segment.ended_at);
        self.revision += 1;
    }

    fn duration_ms(&self) -> u64 {
        self.ended_at.saturating_sub(self.started_at)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConversationTurn {
    pub id: TurnId,
    pub speaker: ConversationSpeaker,
    pub source: AudioSource,
    pub text: String,
    pub utterances: Vec<UtteranceId>,
    pub started_at: AudioTimestamp,
    pub ended_at: AudioTimestamp,
    pub finalized_at: Option<AudioTimestamp>,
}

impl ConversationTurn {
    fn start(id: TurnId, utterance: &ConversationUtterance) -> Self {
        ConversationTurn {
            id,
            speaker: utterance.speaker,
            source: utterance.source,
            text: utterance.text.clone(),
            utterances: vec![utterance.id],
            started_at: utterance.started_at,
            ended_at: utterance.ended_at,
            finalized_at: None,
        }
    }

    fn attach_utterance(&mut self, utterance: &ConversationUtterance) {
        if !self.utterances.contains(&utterance.id) {
            self.utterances.push(utterance.id);
        }
        self.text = join_text(&self.text, &utterance.text);
        self.ended_at = std::cmp::max(self.ended_at, utterance.ended_at);
    }

    fn duration_ms(&self) -> u64 {
        self.ended_at.saturating_sub(self.started_at)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FinalizationReason {
    SpeakerChanged,
    SourceChanged,
    UtteranceGapExceeded,
    TurnInactivityTimeout,
    Paused,
    SessionEnded,
    ManualFlush,
    MaximumUtteranceDuration,
    MaximumTurnDuration,
}

impl FinalizationReason {
    fn as_str(self) -> &'static str {
        match self {
            FinalizationReason::SpeakerChanged => "speaker_changed",
            FinalizationReason::SourceChanged => "source_changed",
            FinalizationReason::UtteranceGapExceeded => "utterance_gap_exceeded",
            FinalizationReason::TurnInactivityTimeout => "turn_inactivity_timeout",
            FinalizationReason::Paused => "paused",
            FinalizationReason::SessionEnded => "session_ended",
            FinalizationReason::ManualFlush => "manual_flush",
            FinalizationReason::MaximumUtteranceDuration => "maximum_utterance_duration",
            FinalizationReason::MaximumTurnDuration => "maximum_turn_duration",
        }
    }

    /// Projeta o motivo interno (que também cobre fechamento de turno) no motivo público
    /// carregado por `ConversationTimelineEvent::UtteranceFinalized`. `UtteranceGapExceeded`
    /// (timer dedicado ou segmento tardio) e `TurnInactivityTimeout` (o turno acima da
    /// utterance estourou o timeout enquanto ela ainda estava aberta) colapsam em
    /// `InactivityTimeout` — do ponto de vista da utterance, ambos são "ninguém falou a
    /// tempo". O mesmo vale para os dois motivos de duração máxima.
    fn to_utterance_reason(self) -> UtteranceFinalizationReason {
        match self {
            FinalizationReason::SpeakerChanged => UtteranceFinalizationReason::SpeakerChanged,
            FinalizationReason::SourceChanged => UtteranceFinalizationReason::SourceChanged,
            FinalizationReason::UtteranceGapExceeded
            | FinalizationReason::TurnInactivityTimeout => {
                UtteranceFinalizationReason::InactivityTimeout
            }
            FinalizationReason::Paused => UtteranceFinalizationReason::CaptureStopped,
            FinalizationReason::SessionEnded => UtteranceFinalizationReason::SessionEnded,
            FinalizationReason::ManualFlush => UtteranceFinalizationReason::ManualFlush,
            FinalizationReason::MaximumUtteranceDuration
            | FinalizationReason::MaximumTurnDuration => {
                UtteranceFinalizationReason::MaximumDuration
            }
        }
    }
}

/// Motivo de finalização de uma `ConversationUtterance`, carregado no evento
/// `UtteranceFinalized` para o `ResponseEngine` e para o painel de diagnóstico em modo
/// dev. `InactivityTimeout` é o caminho esperado para gerar sugestão automaticamente
/// (silêncio após a fala) — `SpeakerChanged`/`SourceChanged` também disparam geração
/// (a pessoa terminou de falar porque alguém mais começou), ver
/// `response_provider::engine::process_conversation_events`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UtteranceFinalizationReason {
    InactivityTimeout,
    SpeakerChanged,
    SourceChanged,
    CaptureStopped,
    ManualFlush,
    SessionEnded,
    MaximumDuration,
}

impl UtteranceFinalizationReason {
    pub fn as_str(self) -> &'static str {
        match self {
            UtteranceFinalizationReason::InactivityTimeout => "inactivity_timeout",
            UtteranceFinalizationReason::SpeakerChanged => "speaker_changed",
            UtteranceFinalizationReason::SourceChanged => "source_changed",
            UtteranceFinalizationReason::CaptureStopped => "capture_stopped",
            UtteranceFinalizationReason::ManualFlush => "manual_flush",
            UtteranceFinalizationReason::SessionEnded => "session_ended",
            UtteranceFinalizationReason::MaximumDuration => "maximum_duration",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConversationTimelineEvent {
    UtteranceStarted {
        utterance_id: UtteranceId,
        turn_id: TurnId,
        speaker: ConversationSpeaker,
        source: AudioSource,
        started_at: AudioTimestamp,
    },
    UtteranceUpdated {
        utterance_id: UtteranceId,
        turn_id: TurnId,
        speaker: ConversationSpeaker,
        source: AudioSource,
        started_at: AudioTimestamp,
        text: String,
        ended_at: AudioTimestamp,
        segments: Vec<SegmentId>,
    },
    UtteranceFinalized {
        turn_id: TurnId,
        utterance: ConversationUtterance,
        finalization_reason: UtteranceFinalizationReason,
        /// `same_speaker_utterance_gap_ms` vigente no momento da finalização (o valor pode
        /// mudar em runtime em modo dev, ver `ConversationTimeline::set_utterance_gap_ms`).
        gap_ms_used: u64,
        /// Silêncio efetivamente observado antes da finalização, em ms, quando mensurável:
        /// para o timer dedicado é o próprio `gap_ms_used` (é por definição o que expirou
        /// sem novo segmento); para o caminho reativo (segmento tardio chegando) é o gap
        /// real entre o fim da utterance e o início do novo segmento. `None` quando a
        /// finalização não tem relação com silêncio (flush manual, troca de speaker/fonte,
        /// captura parada, fim de sessão, duração máxima).
        silence_detected_ms: Option<u64>,
        session_id: SessionId,
    },
    TurnStarted {
        turn_id: TurnId,
        speaker: ConversationSpeaker,
        source: AudioSource,
        started_at: AudioTimestamp,
    },
    TurnUpdated {
        turn: ConversationTurn,
    },
    TurnFinalized {
        turn: ConversationTurn,
        /// Sessão dona deste turno. Sem isso, `process_conversation_events` não teria como
        /// recusar um turno de uma sessão já encerrada ao alimentar o histórico usado no
        /// prompt — era exatamente por aí que a conversa de uma sessão vazava para a
        /// seguinte.
        session_id: SessionId,
    },
    /// Fronteira de sessão: tudo que veio antes pertence a `session_id` e não deve mais
    /// influenciar nada. O frontend usa isso para zerar o que está em tela sem depender de
    /// a janela de sessão ser desmontada.
    SessionEnded {
        session_id: SessionId,
    },
    SessionStarted {
        session_id: SessionId,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct ConversationAssemblerConfig {
    pub same_speaker_utterance_gap_ms: u64,
    pub turn_inactivity_timeout_ms: u64,
    pub maximum_utterance_duration_ms: u64,
    pub maximum_turn_duration_ms: u64,
    pub out_of_order_tolerance_ms: u64,
}

impl Default for ConversationAssemblerConfig {
    fn default() -> Self {
        ConversationAssemblerConfig {
            same_speaker_utterance_gap_ms: 1_800,
            turn_inactivity_timeout_ms: 20_000,
            maximum_utterance_duration_ms: 120_000,
            maximum_turn_duration_ms: 300_000,
            out_of_order_tolerance_ms: 1_000,
        }
    }
}

struct ConversationAssembler {
    config: ConversationAssemblerConfig,
    open_utterance: Option<ConversationUtterance>,
    finalized_utterances: Vec<ConversationUtterance>,
    open_turn: Option<ConversationTurn>,
    finalized_turns: Vec<ConversationTurn>,
    raw_segments: Vec<TranscriptSegment>,
    next_utterance_id: u64,
    next_turn_id: u64,
    last_finalized_ended_at: Option<AudioTimestamp>,
    session_id: SessionId,
}

impl ConversationAssembler {
    fn new(config: ConversationAssemblerConfig) -> Self {
        ConversationAssembler {
            config,
            open_utterance: None,
            finalized_utterances: Vec::new(),
            open_turn: None,
            finalized_turns: Vec::new(),
            raw_segments: Vec::new(),
            next_utterance_id: 0,
            next_turn_id: 0,
            last_finalized_ended_at: None,
            session_id: SessionId::new(),
        }
    }

    fn ingest_segment(&mut self, segment: TranscriptSegment) -> Vec<ConversationTimelineEvent> {
        self.warn_if_out_of_order(&segment);
        self.raw_segments.push(segment.clone());

        let mut events = Vec::new();
        if self.open_utterance.is_none() {
            self.start_utterance_and_maybe_turn(segment, &mut events);
            return events;
        }

        let decision = {
            let utterance = self.open_utterance.as_ref().expect("checked above");
            self.segment_decision(utterance, &segment)
        };

        match decision {
            SegmentDecision::AppendUtterance => {
                let utterance = self.open_utterance.as_mut().expect("open utterance exists");
                utterance.append(&segment);
                let updated_utterance = utterance.clone();
                self.update_open_turn_tail(&updated_utterance);
                events.push(ConversationTimelineEvent::UtteranceUpdated {
                    utterance_id: updated_utterance.id,
                    turn_id: self.open_turn_id(),
                    speaker: updated_utterance.speaker,
                    source: updated_utterance.source,
                    started_at: updated_utterance.started_at,
                    text: updated_utterance.text.clone(),
                    ended_at: updated_utterance.ended_at,
                    segments: updated_utterance.segments.clone(),
                });
                events.push(ConversationTimelineEvent::TurnUpdated {
                    turn: self.open_turn.clone().expect("open turn exists"),
                });
                info!(
                    utterance_id = updated_utterance.id.0,
                    turn_id = self.open_turn_id().0,
                    segments = updated_utterance.segments.len(),
                    duration_ms = updated_utterance.duration_ms(),
                    "conversation utterance updated"
                );

                if updated_utterance.duration_ms() >= self.config.maximum_utterance_duration_ms {
                    events.extend(self.finalize_open_utterance(
                        FinalizationReason::MaximumUtteranceDuration,
                        None,
                    ));
                }
                if self
                    .open_turn
                    .as_ref()
                    .is_some_and(|turn| turn.duration_ms() >= self.config.maximum_turn_duration_ms)
                {
                    events.extend(self.finalize_open_turn(FinalizationReason::MaximumTurnDuration));
                }
            }
            SegmentDecision::NewUtteranceSameTurn(reason) => {
                let observed_gap = self
                    .open_utterance
                    .as_ref()
                    .map(|u| segment.started_at.saturating_sub(u.ended_at));
                events.extend(self.finalize_open_utterance(reason, observed_gap));
                self.start_utterance_and_maybe_turn(segment, &mut events);
            }
            SegmentDecision::NewTurn(reason) => {
                let observed_gap = self
                    .open_utterance
                    .as_ref()
                    .map(|u| segment.started_at.saturating_sub(u.ended_at));
                events.extend(self.finalize_open_utterance(reason, observed_gap));
                events.extend(self.finalize_open_turn(reason));
                self.start_utterance_and_maybe_turn(segment, &mut events);
            }
        }

        events
    }

    fn segment_decision(
        &self,
        utterance: &ConversationUtterance,
        segment: &TranscriptSegment,
    ) -> SegmentDecision {
        if utterance.speaker != segment.speaker {
            return SegmentDecision::NewTurn(FinalizationReason::SpeakerChanged);
        }
        if utterance.source != segment.source {
            return SegmentDecision::NewTurn(FinalizationReason::SourceChanged);
        }
        if segment.started_at < utterance.ended_at {
            let skew = utterance.ended_at.saturating_sub(segment.started_at);
            if skew > self.config.out_of_order_tolerance_ms {
                return SegmentDecision::NewUtteranceSameTurn(
                    FinalizationReason::UtteranceGapExceeded,
                );
            }
        }

        let gap = segment.started_at.saturating_sub(utterance.ended_at);
        if gap > self.config.turn_inactivity_timeout_ms {
            return SegmentDecision::NewTurn(FinalizationReason::TurnInactivityTimeout);
        }
        if gap > self.config.same_speaker_utterance_gap_ms {
            return SegmentDecision::NewUtteranceSameTurn(FinalizationReason::UtteranceGapExceeded);
        }

        let resulting_utterance_duration = std::cmp::max(utterance.ended_at, segment.ended_at)
            .saturating_sub(utterance.started_at);
        if resulting_utterance_duration > self.config.maximum_utterance_duration_ms {
            return SegmentDecision::NewUtteranceSameTurn(
                FinalizationReason::MaximumUtteranceDuration,
            );
        }

        if let Some(turn) = &self.open_turn {
            let resulting_turn_duration =
                std::cmp::max(turn.ended_at, segment.ended_at).saturating_sub(turn.started_at);
            if resulting_turn_duration > self.config.maximum_turn_duration_ms {
                return SegmentDecision::NewTurn(FinalizationReason::MaximumTurnDuration);
            }
        }

        SegmentDecision::AppendUtterance
    }

    fn start_utterance_and_maybe_turn(
        &mut self,
        segment: TranscriptSegment,
        events: &mut Vec<ConversationTimelineEvent>,
    ) {
        self.next_utterance_id += 1;
        let utterance = ConversationUtterance::start(UtteranceId(self.next_utterance_id), &segment);

        if let Some(turn) = self.open_turn.as_mut() {
            turn.attach_utterance(&utterance);
        } else {
            self.next_turn_id += 1;
            let turn = ConversationTurn::start(TurnId(self.next_turn_id), &utterance);
            info!(
                turn_id = turn.id.0,
                speaker = ?turn.speaker,
                utterance_id = utterance.id.0,
                segment_id = ?segment.segment_id,
                "conversation turn started"
            );
            events.push(ConversationTimelineEvent::TurnStarted {
                turn_id: turn.id,
                speaker: turn.speaker,
                source: turn.source,
                started_at: turn.started_at,
            });
            self.open_turn = Some(turn);
        }

        let turn_id = self.open_turn_id();
        info!(
            utterance_id = utterance.id.0,
            turn_id = turn_id.0,
            speaker = ?utterance.speaker,
            segment_id = ?segment.segment_id,
            "conversation utterance started"
        );
        events.push(ConversationTimelineEvent::UtteranceStarted {
            utterance_id: utterance.id,
            turn_id,
            speaker: utterance.speaker,
            source: utterance.source,
            started_at: utterance.started_at,
        });
        events.push(ConversationTimelineEvent::UtteranceUpdated {
            utterance_id: utterance.id,
            turn_id,
            speaker: utterance.speaker,
            source: utterance.source,
            started_at: utterance.started_at,
            text: utterance.text.clone(),
            ended_at: utterance.ended_at,
            segments: utterance.segments.clone(),
        });
        events.push(ConversationTimelineEvent::TurnUpdated {
            turn: self.open_turn.clone().expect("open turn exists"),
        });

        self.open_utterance = Some(utterance);

        if segment.ended_at.saturating_sub(segment.started_at)
            >= self.config.maximum_utterance_duration_ms
        {
            events.extend(
                self.finalize_open_utterance(FinalizationReason::MaximumUtteranceDuration, None),
            );
        }
    }

    fn update_open_turn_tail(&mut self, utterance: &ConversationUtterance) {
        let finalized_utterances = self.finalized_utterances.clone();
        let turn = self.open_turn.as_mut().expect("open turn exists");
        if turn.utterances.last().copied() != Some(utterance.id) {
            turn.utterances.push(utterance.id);
        }
        turn.text = build_turn_text(&turn.utterances, &finalized_utterances, Some(utterance));
        turn.ended_at = std::cmp::max(turn.ended_at, utterance.ended_at);
    }

    fn finalize_open_utterance(
        &mut self,
        reason: FinalizationReason,
        observed_gap_ms: Option<u64>,
    ) -> Vec<ConversationTimelineEvent> {
        let Some(mut utterance) = self.open_utterance.take() else {
            return Vec::new();
        };
        utterance.finalized_at = Some(utterance.ended_at);
        self.update_open_turn_tail(&utterance);
        info!(
            utterance_id = utterance.id.0,
            turn_id = self.open_turn_id().0,
            speaker = ?utterance.speaker,
            segments = utterance.segments.len(),
            reason = reason.as_str(),
            "conversation utterance finalized"
        );
        self.finalized_utterances.push(utterance.clone());
        vec![
            ConversationTimelineEvent::UtteranceFinalized {
                turn_id: self.open_turn_id(),
                utterance,
                finalization_reason: reason.to_utterance_reason(),
                gap_ms_used: self.config.same_speaker_utterance_gap_ms,
                silence_detected_ms: observed_gap_ms,
                session_id: self.session_id,
            },
            ConversationTimelineEvent::TurnUpdated {
                turn: self.open_turn.clone().expect("open turn exists"),
            },
        ]
    }

    /// Identidade (id + revisão) da utterance aberta, se houver — usada por
    /// `ConversationTimeline::reschedule_utterance_timer` para saber o que agendar, e como
    /// chave de comparação em `finalize_utterance_if_stale` para decidir se o timer que
    /// expirou ainda corresponde ao estado atual.
    fn open_utterance_identity(&self) -> Option<(UtteranceId, u64)> {
        self.open_utterance.as_ref().map(|u| (u.id, u.revision))
    }

    /// Chamado quando o timer dedicado de uma utterance expira. Só finaliza se a
    /// utterance aberta ainda for exatamente a mesma (mesmo id e revisão) que estava
    /// aberta quando o timer foi agendado — qualquer segmento novo (mesmo turno ou não),
    /// flush, parada de captura ou fim de sessão já terá mudado a revisão ou zerado
    /// `open_utterance`, tornando este disparo um no-op em vez de uma finalização
    /// duplicada ou incorreta.
    fn finalize_utterance_if_stale(
        &mut self,
        utterance_id: UtteranceId,
        revision: u64,
    ) -> Vec<ConversationTimelineEvent> {
        let still_current = self
            .open_utterance
            .as_ref()
            .is_some_and(|u| u.id == utterance_id && u.revision == revision);
        if !still_current {
            return Vec::new();
        }
        let gap_ms = self.config.same_speaker_utterance_gap_ms;
        self.finalize_open_utterance(FinalizationReason::UtteranceGapExceeded, Some(gap_ms))
    }

    fn finalize_open_turn(&mut self, reason: FinalizationReason) -> Vec<ConversationTimelineEvent> {
        let Some(mut turn) = self.open_turn.take() else {
            return Vec::new();
        };
        turn.finalized_at = Some(turn.ended_at);
        self.last_finalized_ended_at = Some(turn.ended_at);
        info!(
            turn_id = turn.id.0,
            speaker = ?turn.speaker,
            utterances = turn.utterances.len(),
            reason = reason.as_str(),
            "conversation turn finalized"
        );
        self.finalized_turns.push(turn.clone());
        vec![ConversationTimelineEvent::TurnFinalized {
            turn,
            session_id: self.session_id,
        }]
    }

    /// Descarta *todo* o conteúdo conversacional e abre uma nova sessão. Só a configuração
    /// (gaps/timeouts) sobrevive — ela é preferência do usuário, não conversa.
    ///
    /// `next_utterance_id`/`next_turn_id` continuam de onde estavam de propósito: ids
    /// únicos por processo tornam impossível um evento atrasado da sessão anterior casar
    /// com um turno/utterance da nova por coincidência de id (a identidade lógica é
    /// `session_id + turn_id + utterance_id`, ver `docs/response-suggestion.md`).
    fn reset_for_new_session(&mut self) -> SessionId {
        let previous_session_id = self.session_id;
        let discarded_turns = self.finalized_turns.len() + usize::from(self.open_turn.is_some());
        let discarded_utterances =
            self.finalized_utterances.len() + usize::from(self.open_utterance.is_some());
        self.open_utterance = None;
        self.finalized_utterances.clear();
        self.open_turn = None;
        self.finalized_turns.clear();
        self.raw_segments.clear();
        self.last_finalized_ended_at = None;
        self.session_id = SessionId::new();
        info!(
            previous_session_id = previous_session_id.0,
            new_session_id = self.session_id.0,
            discarded_turns,
            discarded_utterances,
            "session_state_cleared"
        );
        self.session_id
    }

    fn finalize_source(
        &mut self,
        source: AudioSource,
        reason: FinalizationReason,
    ) -> Vec<ConversationTimelineEvent> {
        if self
            .open_turn
            .as_ref()
            .is_some_and(|turn| turn.source == source)
        {
            let mut events = self.finalize_open_utterance(reason, None);
            events.extend(self.finalize_open_turn(reason));
            events
        } else {
            Vec::new()
        }
    }

    fn flush(&mut self) -> Vec<ConversationTimelineEvent> {
        let mut events = self.finalize_open_utterance(FinalizationReason::ManualFlush, None);
        events.extend(self.finalize_open_turn(FinalizationReason::ManualFlush));
        events
    }

    fn end_session(&mut self) -> Vec<ConversationTimelineEvent> {
        let mut events = self.finalize_open_utterance(FinalizationReason::SessionEnded, None);
        events.extend(self.finalize_open_turn(FinalizationReason::SessionEnded));
        events
    }

    fn snapshot(&self) -> ConversationTimelineSnapshot {
        let mut turns = self.finalized_turns.clone();
        if let Some(open) = self.open_turn.clone() {
            turns.push(open);
        }
        turns.sort_by_key(|turn| (turn.started_at, turn.ended_at, turn.id));

        let mut utterances = self.finalized_utterances.clone();
        if let Some(open) = self.open_utterance.clone() {
            utterances.push(open);
        }
        utterances
            .sort_by_key(|utterance| (utterance.started_at, utterance.ended_at, utterance.id));

        ConversationTimelineSnapshot { turns, utterances }
    }

    fn raw_segments(&self) -> Vec<TranscriptSegment> {
        let mut segments = self.raw_segments.clone();
        segments.sort_by_key(|segment| (segment.started_at, segment.received_sequence));
        segments
    }

    fn open_turn_id(&self) -> TurnId {
        self.open_turn.as_ref().expect("open turn exists").id
    }

    fn warn_if_out_of_order(&self, segment: &TranscriptSegment) {
        if let Some(open) = &self.open_utterance {
            if segment.started_at < open.ended_at {
                let skew = open.ended_at.saturating_sub(segment.started_at);
                warn!(
                    segment_id = ?segment.segment_id,
                    utterance_id = open.id.0,
                    skew_ms = skew,
                    tolerance_ms = self.config.out_of_order_tolerance_ms,
                    "out-of-order transcript segment received while utterance is open"
                );
            }
        }
        if let Some(last_finalized) = self.last_finalized_ended_at {
            if segment.started_at < last_finalized {
                let skew = last_finalized.saturating_sub(segment.started_at);
                warn!(
                    segment_id = ?segment.segment_id,
                    skew_ms = skew,
                    tolerance_ms = self.config.out_of_order_tolerance_ms,
                    "out-of-order transcript segment received after finalized turn"
                );
            }
        }
    }
}

enum SegmentDecision {
    AppendUtterance,
    NewUtteranceSameTurn(FinalizationReason),
    NewTurn(FinalizationReason),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConversationTimelineSnapshot {
    pub turns: Vec<ConversationTurn>,
    pub utterances: Vec<ConversationUtterance>,
}

/// Recebe eventos produzidos fora do fluxo síncrono de `ingest_transcript_event` — hoje,
/// só o timer dedicado da utterance (ver `reschedule_utterance_timer`). O chamador (
/// `lib.rs`) registra aqui a mesma sequência `emit_conversation_events` +
/// `process_conversation_events` que já usa manualmente para eventos síncronos, para que
/// uma utterance finalizada por silêncio chegue ao frontend e ao `ResponseEngine` do
/// mesmo jeito que uma finalizada por um novo segmento.
pub type ConversationEventSink = Arc<dyn Fn(Vec<ConversationTimelineEvent>) + Send + Sync>;

/// Resultado de uma transição de sessão. `events` já vem na ordem em que deve chegar ao
/// frontend (finalizações da sessão que acabou, depois a fronteira).
pub struct SessionTransition {
    pub previous_session_id: SessionId,
    pub session_id: SessionId,
    pub events: Vec<ConversationTimelineEvent>,
}

pub struct ConversationTimeline {
    assembler: Mutex<ConversationAssembler>,
    next_sequence: AtomicU64,
    sink: OnceLock<ConversationEventSink>,
    /// Token raiz da sessão atual: cancelado (e substituído por um novo) em toda
    /// transição de sessão, para que nenhum timer de utterance agendado na sessão
    /// anterior chegue a rodar. A checagem de `session_id`/revisão em
    /// `fire_utterance_timeout` já tornaria o timer um no-op, mas cancelar é o que
    /// garante que a task nem acorda — ver `docs/response-suggestion.md`.
    session_cancel: Mutex<CancellationToken>,
    /// Referência fraca a si mesmo, usada para o timer dedicado poder chamar de volta em
    /// `fire_utterance_timeout` depois de `tokio::time::sleep`. Só fica populada depois de
    /// `attach()`; sem isso (app encerrando, ou testes que não chamam `attach`), o timer
    /// simplesmente não é agendado — nunca panica.
    self_weak: OnceLock<Weak<ConversationTimeline>>,
}

impl Default for ConversationTimeline {
    fn default() -> Self {
        ConversationTimeline::new(ConversationAssemblerConfig::default())
    }
}

impl ConversationTimeline {
    pub fn new(config: ConversationAssemblerConfig) -> Self {
        ConversationTimeline {
            assembler: Mutex::new(ConversationAssembler::new(config)),
            next_sequence: AtomicU64::new(0),
            sink: OnceLock::new(),
            session_cancel: Mutex::new(CancellationToken::new()),
            self_weak: OnceLock::new(),
        }
    }

    /// Precisa ser chamado uma vez, logo após envolver o timeline em `Arc`, para que o
    /// timer dedicado da utterance consiga se auto-referenciar. Sem isso o timer nunca é
    /// agendado (finalização volta a depender só de eventos externos).
    pub fn attach(self: &Arc<Self>) {
        let _ = self.self_weak.set(Arc::downgrade(self));
    }

    pub fn set_event_sink(&self, sink: ConversationEventSink) {
        let _ = self.sink.set(sink);
    }

    /// Sessão ativa neste momento. Todo evento/tarefa que se refira à conversa carrega
    /// uma `SessionId` e é comparado com esta antes de produzir qualquer efeito.
    pub fn session_id(&self) -> SessionId {
        self.assembler
            .lock()
            .expect("conversation timeline mutex poisoned")
            .session_id
    }

    /// Cancela o token da sessão que está saindo e instala um token raiz novo. Nunca
    /// reaproveita um token já cancelado.
    fn rotate_session_token(&self) {
        let mut token = self
            .session_cancel
            .lock()
            .expect("conversation timeline mutex poisoned");
        token.cancel();
        *token = CancellationToken::new();
    }

    fn session_token(&self) -> CancellationToken {
        self.session_cancel
            .lock()
            .expect("conversation timeline mutex poisoned")
            .clone()
    }

    /// Abre uma sessão nova, descartando qualquer resto da anterior (utterance/turno
    /// aberto incluídos — são descartados, não finalizados: pertencem a uma conversa que
    /// não existe mais). Idempotente no sentido que importa: chamar duas vezes só produz
    /// outra fronteira sobre um estado já vazio.
    pub fn start_session(&self) -> SessionTransition {
        self.rotate_session_token();
        let (previous_session_id, session_id) = {
            let mut assembler = self
                .assembler
                .lock()
                .expect("conversation timeline mutex poisoned");
            let previous = assembler.session_id;
            (previous, assembler.reset_for_new_session())
        };
        info!(
            session_id = session_id.0,
            previous_session_id = previous_session_id.0,
            "session_started"
        );
        SessionTransition {
            previous_session_id,
            session_id,
            events: vec![ConversationTimelineEvent::SessionStarted { session_id }],
        }
    }

    /// `same_speaker_utterance_gap_ms` atualmente em uso — configurável em runtime via
    /// `set_utterance_gap_ms` (modo dev, sem rebuild).
    pub fn utterance_gap_ms(&self) -> u64 {
        self.assembler
            .lock()
            .expect("conversation timeline mutex poisoned")
            .config
            .same_speaker_utterance_gap_ms
    }

    pub fn set_utterance_gap_ms(&self, gap_ms: u64) {
        self.assembler
            .lock()
            .expect("conversation timeline mutex poisoned")
            .config
            .same_speaker_utterance_gap_ms = gap_ms;
    }

    /// Caminho de produção: um resultado final **já validado e normalizado** pelo
    /// `TranscriptionRuntime`. A timeline não faz filtro de sessão aqui de propósito — quando
    /// este método é chamado, o runtime já garantiu que o resultado pertence à sessão ativa,
    /// à sessão de transcrição viva daquela fonte e não é reentrega.
    pub fn ingest_normalized_transcript(
        &self,
        transcript: &FinalTranscript,
        normalization: &TranscriptNormalizationResult,
    ) -> Vec<ConversationTimelineEvent> {
        let sequence = self.next_sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let Some(segment) = TranscriptSegment::from_normalized(transcript, normalization, sequence)
        else {
            return Vec::new();
        };
        self.ingest_segment(segment)
    }

    /// Entrada **de teste**, sem normalização: exercita a montagem de utterances/turnos a
    /// partir de texto cru, isolando esses testes de mudanças no vocabulário técnico. O
    /// caminho de produção é `ingest_normalized_transcript`.
    #[cfg(test)]
    pub fn ingest_transcript_event(
        &self,
        event: TranscriptEvent,
    ) -> Vec<ConversationTimelineEvent> {
        let TranscriptEvent::Ready(transcript) = event else {
            return Vec::new();
        };

        let sequence = self.next_sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let Some(segment) = TranscriptSegment::from_transcript(transcript, sequence) else {
            return Vec::new();
        };
        self.ingest_segment(segment)
    }

    fn ingest_segment(&self, segment: TranscriptSegment) -> Vec<ConversationTimelineEvent> {
        let events = self
            .assembler
            .lock()
            .expect("conversation timeline mutex poisoned")
            .ingest_segment(segment);
        self.reschedule_utterance_timer();
        events
    }

    /// (Re)agenda o timer dedicado da utterance aberta, se houver uma. Chamado após todo
    /// `ingest_transcript_event` — item 4 do plano de correção: cada segmento novo cancela
    /// implicitamente qualquer timer anterior (via a checagem de revisão em
    /// `finalize_utterance_if_stale`, não via cancelamento explícito da task) e agenda um
    /// novo com a revisão atual. Se não houver utterance aberta, ou o timeline ainda não
    /// tiver sido `attach()`ado, não faz nada.
    fn reschedule_utterance_timer(&self) {
        let (identity, gap_ms, session_id) = {
            let assembler = self
                .assembler
                .lock()
                .expect("conversation timeline mutex poisoned");
            (
                assembler.open_utterance_identity(),
                assembler.config.same_speaker_utterance_gap_ms,
                assembler.session_id,
            )
        };
        let Some((utterance_id, revision)) = identity else {
            return;
        };
        let Some(timeline) = self.self_weak.get().and_then(Weak::upgrade) else {
            return;
        };
        let session_token = self.session_token();

        tokio::spawn(async move {
            tokio::select! {
                _ = session_token.cancelled() => {
                    debug!(
                        session_id = session_id.0,
                        utterance_id = utterance_id.0,
                        "utterance timer dropped: session ended before it fired"
                    );
                }
                _ = tokio::time::sleep(Duration::from_millis(gap_ms)) => {
                    timeline.fire_utterance_timeout(session_id, utterance_id, revision);
                }
            }
        });
    }

    /// Executado quando um timer de utterance expira. Só produz efeito se ainda estivermos
    /// na mesma sessão *e* a utterance aberta ainda for a mesma (mesmo id + revisão) que
    /// estava aberta quando o timer foi agendado — ver
    /// `ConversationAssembler::finalize_utterance_if_stale`. A checagem de sessão é o que
    /// impede um timer agendado na sessão A de finalizar (e disparar geração para) uma
    /// utterance depois que a sessão B começou.
    fn fire_utterance_timeout(
        &self,
        session_id: SessionId,
        utterance_id: UtteranceId,
        revision: u64,
    ) {
        let events = {
            let mut assembler = self
                .assembler
                .lock()
                .expect("conversation timeline mutex poisoned");
            if assembler.session_id != session_id {
                debug!(
                    timer_session_id = session_id.0,
                    active_session_id = assembler.session_id.0,
                    utterance_id = utterance_id.0,
                    "utterance timer discarded: belongs to a finished session"
                );
                return;
            }
            assembler.finalize_utterance_if_stale(utterance_id, revision)
        };
        if events.is_empty() {
            return;
        }
        if let Some(sink) = self.sink.get() {
            sink(events);
        }
    }

    pub fn ingest_capture_event(
        &self,
        event: &AudioCaptureEvent,
    ) -> Vec<ConversationTimelineEvent> {
        let AudioCaptureEvent::Stopped { source } = event else {
            return Vec::new();
        };
        self.assembler
            .lock()
            .expect("conversation timeline mutex poisoned")
            .finalize_source(*source, FinalizationReason::Paused)
    }

    pub fn flush(&self) -> Vec<ConversationTimelineEvent> {
        self.assembler
            .lock()
            .expect("conversation timeline mutex poisoned")
            .flush()
    }

    /// Encerra a sessão ativa: finaliza o que estava aberto (para o frontend ver a
    /// conversa fechada), cancela os timers em voo, apaga *todo* o estado conversacional e
    /// já abre a fronteira da próxima sessão. Os eventos devolvidos pertencem à sessão que
    /// acabou e **não** devem ser reprocessados pelo `ResponseEngine` — ver
    /// `conversation_end_session_command`.
    pub fn end_session(&self) -> SessionTransition {
        let previous_session_id = self.session_id();
        info!(session_id = previous_session_id.0, "session_ending");

        let mut events = self
            .assembler
            .lock()
            .expect("conversation timeline mutex poisoned")
            .end_session();
        events.push(ConversationTimelineEvent::SessionEnded {
            session_id: previous_session_id,
        });

        self.rotate_session_token();
        let session_id = self
            .assembler
            .lock()
            .expect("conversation timeline mutex poisoned")
            .reset_for_new_session();
        events.push(ConversationTimelineEvent::SessionStarted { session_id });
        info!(
            session_id = previous_session_id.0,
            next_session_id = session_id.0,
            "session_ended"
        );

        SessionTransition {
            previous_session_id,
            session_id,
            events,
        }
    }

    pub fn snapshot(&self) -> ConversationTimelineSnapshot {
        self.assembler
            .lock()
            .expect("conversation timeline mutex poisoned")
            .snapshot()
    }

    pub fn raw_segments(&self) -> Vec<TranscriptSegment> {
        self.assembler
            .lock()
            .expect("conversation timeline mutex poisoned")
            .raw_segments()
    }
}

pub struct ConversationTimelineState(pub Arc<ConversationTimeline>);

#[tauri::command]
pub async fn conversation_timeline_snapshot_command(
    state: State<'_, ConversationTimelineState>,
) -> Result<ConversationTimelineSnapshot, String> {
    Ok(state.0.snapshot())
}

#[tauri::command]
pub async fn conversation_flush_turns_command(
    app: AppHandle,
    state: State<'_, ConversationTimelineState>,
    response_state: State<'_, ResponseEngineState>,
) -> Result<(), String> {
    let events = state.0.flush();
    emit_conversation_events(&app, events.clone());
    process_conversation_events(&app, response_state.0.clone(), &events);
    Ok(())
}

/// Abre uma sessão nova e vazia. Chamado pelo frontend ao iniciar uma sessão, antes de
/// qualquer captura: garante a fronteira mesmo que a sessão anterior tenha terminado por
/// um caminho que não passou por `conversation_end_session_command`.
#[tauri::command]
pub async fn conversation_start_session_command(
    app: AppHandle,
    state: State<'_, ConversationTimelineState>,
    response_state: State<'_, ResponseEngineState>,
    transcription_state: State<'_, crate::transcription::TranscriptionState>,
) -> Result<(), String> {
    let engine = response_state.0.clone();
    // Ordem: transcrição primeiro. Encerrar a camada que *produz* fala antes de encerrar as
    // que a consomem é o que garante que nenhum resultado em voo entre na sessão nova —
    // fazer o contrário deixaria uma janela em que a timeline já rodou para a próxima
    // sessão enquanto um resultado da anterior ainda está a caminho dela.
    transcription_state.runtime.end_session().await;
    // Encerra o que estiver ativo no motor antes de trocar a fronteira: cancela geração em
    // andamento e limpa contexto da sessão anterior.
    engine.end_session(state.0.session_id());
    let transition = state.0.start_session();
    engine.begin_session(transition.session_id);
    transcription_state
        .runtime
        .begin_session(transition.session_id)
        .await;
    info!(
        previous_session_id = transition.previous_session_id.value(),
        session_id = transition.session_id.value(),
        "conversation session boundary opened"
    );
    emit_conversation_events(&app, transition.events);
    Ok(())
}

/// Encerramento atômico e ordenado: marca a sessão como encerrando (nenhum gatilho novo é
/// aceito), cancela a geração ativa e os timers, apaga contexto/turnos/utterances/segmentos
/// e só então abre a fronteira da próxima sessão. Os eventos de finalização são emitidos
/// ao frontend, mas **não** passam por `process_conversation_events` — reprocessá-los era
/// justamente o que fazia a última fala da sessão encerrada gerar uma sugestão que
/// aparecia na sessão seguinte. Idempotente: encerrar duas vezes só troca a fronteira de
/// novo, sobre um estado já vazio.
#[tauri::command]
pub async fn conversation_end_session_command(
    app: AppHandle,
    state: State<'_, ConversationTimelineState>,
    response_state: State<'_, ResponseEngineState>,
    transcription_state: State<'_, crate::transcription::TranscriptionState>,
) -> Result<(), String> {
    let engine = response_state.0.clone();
    // Transcrição primeiro, pela mesma razão de `conversation_start_session_command`:
    // bloquear a produção de fala nova antes de apagar quem a consome.
    transcription_state.runtime.end_session().await;
    engine.end_session(state.0.session_id());
    let transition = state.0.end_session();
    info!(
        previous_session_id = transition.previous_session_id.value(),
        session_id = transition.session_id.value(),
        "conversation session boundary closed"
    );
    emit_conversation_events(&app, transition.events);
    engine.begin_session(transition.session_id);
    // A timeline não fica sem sessão depois de encerrar: `end_session` já rotaciona para a
    // próxima. Alinhar o runtime de transcrição a essa fronteira nova é o que impede que
    // toda transcrição posterior a um "encerrar sessão" fosse descartada por não haver
    // sessão de transcrição ativa.
    transcription_state
        .runtime
        .begin_session(transition.session_id)
        .await;
    Ok(())
}

/// Dispara manualmente uma sugestão de resposta para `turn_id` (botão "Regenerar" e
/// atalho Ctrl/Cmd+Shift+Enter no frontend — ver `docs/shortcuts.md`). Não é lógica de
/// geração nova: reusa exatamente `ResponseEngine::trigger_generation`, o mesmo caminho
/// que `process_conversation_events` já chama automaticamente em `UtteranceFinalized`
/// (cancela uma geração em andamento para o turno do mesmo jeito, emite os mesmos
/// eventos, aparece com `finalization_reason: "manual_regenerate"` no diagnóstico). Só
/// existe porque, sem geração automática nova para se ancorar, o usuário precisa de uma
/// forma explícita de pedir uma sugestão de novo.
#[tauri::command]
pub async fn conversation_regenerate_suggestion_command(
    app: AppHandle,
    turn_id: u64,
    state: State<'_, ConversationTimelineState>,
    response_state: State<'_, ResponseEngineState>,
) -> Result<(), String> {
    let session_id = state.0.session_id();
    let snapshot = state.0.snapshot();
    let turn = snapshot
        .turns
        .iter()
        .find(|t| t.id.value() == turn_id)
        .cloned()
        .ok_or_else(|| "turno não encontrado".to_string())?;
    if !is_eligible_turn(&turn) {
        return Err("turno não é elegível para sugestão de resposta".to_string());
    }
    // A "fala atual" de uma regeneração é a última utterance do turno, não o turno
    // inteiro: é o mesmo recorte que o gatilho automático usa, para o modelo decidir sobre
    // a fala mais recente e não sobre tudo que a pessoa já disse enquanto tinha a palavra.
    let last_utterance = turn
        .utterances
        .last()
        .and_then(|id| snapshot.utterances.iter().find(|u| u.id == *id))
        .ok_or_else(|| "turno não tem utterance para regenerar".to_string())?;
    let trigger = GenerationTrigger {
        session_id,
        utterance_id: last_utterance.id,
        utterance_revision: last_utterance.revision,
        utterance_text: last_utterance.text.clone(),
        utterance_finalized_at: Instant::now(),
        finalization_reason: "manual_regenerate".to_string(),
        gap_ms_used: 0,
        silence_detected_ms: None,
    };
    response_state
        .0
        .clone()
        .trigger_generation(app, turn, trigger);
    Ok(())
}

#[tauri::command]
pub async fn conversation_raw_segments_command(
    state: State<'_, ConversationTimelineState>,
) -> Result<Vec<TranscriptSegment>, String> {
    Ok(state.0.raw_segments())
}

/// `same_speaker_utterance_gap_ms` atual. Exposto para o painel de dev (`import.meta.env.DEV`
/// no frontend) testar valores diferentes sem rebuild — ver `docs/response-suggestion.md`.
#[tauri::command]
pub async fn conversation_get_utterance_gap_ms_command(
    state: State<'_, ConversationTimelineState>,
) -> Result<u64, String> {
    Ok(state.0.utterance_gap_ms())
}

#[tauri::command]
pub async fn conversation_set_utterance_gap_ms_command(
    state: State<'_, ConversationTimelineState>,
    gap_ms: u64,
) -> Result<(), String> {
    state.0.set_utterance_gap_ms(gap_ms);
    Ok(())
}

pub fn emit_conversation_events(app: &AppHandle, events: Vec<ConversationTimelineEvent>) {
    for event in events {
        record_utterance_telemetry(&event);
        if let Err(e) = app.emit(CONVERSATION_TIMELINE_EVENT, &event) {
            warn!(%e, "failed to emit conversation timeline event to frontend");
        }
    }
}

/// Liga o trace da fala (aberto lá atrás no primeiro chunk de áudio) ao `UtteranceId` que
/// só passa a existir aqui, e grava o marco `utterance_finalized`.
///
/// Feito neste funil, e não dentro do assembler, porque `emit_conversation_events` é o único
/// ponto por onde **todos** os caminhos de finalização passam: comando Tauri, ingestão de
/// segmento e o timer dedicado da utterance. Marcar dentro do assembler exigiria repetir a
/// chamada em cada um deles, e o caminho do timer — justamente o mais importante para
/// latência — é o mais fácil de esquecer.
fn record_utterance_telemetry(event: &ConversationTimelineEvent) {
    let ConversationTimelineEvent::UtteranceFinalized { utterance, .. } = event else {
        return;
    };
    // O último segmento é o que fecha a fala: é o instante dele que separa "parou de falar"
    // de "a utterance foi declarada terminada".
    let Some(last_segment) = utterance.segments.last() else {
        return;
    };
    let recorder = crate::telemetry::recorder();
    let Some(trace) = recorder.trace_for_segment(*last_segment) else {
        return;
    };
    recorder.link_utterance(trace, utterance.id);
    recorder.mark(trace, crate::telemetry::Milestone::UtteranceFinalized);
}

fn normalize_segment_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn join_text(current: &str, next: &str) -> String {
    let current = normalize_segment_text(current);
    let next = normalize_segment_text(next);
    if current.is_empty() {
        next
    } else if next.is_empty() {
        current
    } else {
        format!("{current} {next}")
    }
}

fn build_turn_text(
    utterance_ids: &[UtteranceId],
    finalized: &[ConversationUtterance],
    open: Option<&ConversationUtterance>,
) -> String {
    let mut text = String::new();
    for id in utterance_ids {
        let utterance = finalized
            .iter()
            .find(|u| u.id == *id)
            .or_else(|| open.filter(|u| u.id == *id));
        if let Some(utterance) = utterance {
            text = join_text(&text, &utterance.text);
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transcript(source: AudioSource, text: &str, start: u64, end: u64) -> Transcript {
        Transcript {
            segment_id: SegmentId::next(),
            source,
            text: text.into(),
            language: Some("pt".into()),
            started_at: AudioTimestamp(start),
            ended_at: AudioTimestamp(end),
            processing_time_ms: 12,
        }
    }

    fn assembler() -> ConversationAssembler {
        ConversationAssembler::new(ConversationAssemblerConfig::default())
    }

    fn segment(source: AudioSource, text: &str, start: u64, end: u64) -> TranscriptSegment {
        TranscriptSegment::from_transcript(transcript(source, text, start, end), 1).unwrap()
    }

    fn finalized_turns(events: &[ConversationTimelineEvent]) -> Vec<&ConversationTurn> {
        events
            .iter()
            .filter_map(|event| match event {
                ConversationTimelineEvent::TurnFinalized { turn, .. } => Some(turn),
                _ => None,
            })
            .collect()
    }

    fn finalized_utterances(events: &[ConversationTimelineEvent]) -> Vec<&ConversationUtterance> {
        events
            .iter()
            .filter_map(|event| match event {
                ConversationTimelineEvent::UtteranceFinalized { utterance, .. } => Some(utterance),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn maps_sources_to_conversation_speakers() {
        assert_eq!(
            ConversationSpeaker::from(AudioSource::Microphone),
            ConversationSpeaker::User
        );
        assert_eq!(
            ConversationSpeaker::from(AudioSource::SystemOutput),
            ConversationSpeaker::OtherPerson
        );
    }

    #[test]
    fn nearby_segments_form_one_utterance() {
        let mut assembler = assembler();
        assembler.ingest_segment(segment(AudioSource::Microphone, "Um.", 0, 500));
        assembler.ingest_segment(segment(AudioSource::Microphone, "Dois.", 900, 1_200));

        let snapshot = assembler.snapshot();
        assert_eq!(snapshot.utterances.len(), 1);
        assert_eq!(snapshot.utterances[0].segments.len(), 2);
        assert_eq!(snapshot.utterances[0].text, "Um. Dois.");
    }

    #[test]
    fn short_gap_exceeded_creates_another_utterance() {
        let mut assembler = assembler();
        assembler.ingest_segment(segment(AudioSource::Microphone, "Um.", 0, 500));
        let events =
            assembler.ingest_segment(segment(AudioSource::Microphone, "Dois.", 2_500, 2_900));

        assert_eq!(finalized_utterances(&events).len(), 1);
        let snapshot = assembler.snapshot();
        assert_eq!(snapshot.utterances.len(), 2);
    }

    #[test]
    fn two_utterances_from_same_speaker_remain_in_one_turn() {
        let mut assembler = assembler();
        assembler.ingest_segment(segment(AudioSource::SystemOutput, "Primeira.", 0, 500));
        assembler.ingest_segment(segment(AudioSource::SystemOutput, "Segunda.", 3_000, 3_400));

        let snapshot = assembler.snapshot();
        assert_eq!(snapshot.utterances.len(), 2);
        assert_eq!(snapshot.turns.len(), 1);
        assert_eq!(snapshot.turns[0].utterances.len(), 2);
        assert_eq!(snapshot.turns[0].text, "Primeira. Segunda.");
    }

    #[test]
    fn three_second_pause_does_not_end_turn() {
        let mut assembler = assembler();
        assembler.ingest_segment(segment(AudioSource::SystemOutput, "Primeira.", 0, 500));
        assembler.ingest_segment(segment(
            AudioSource::SystemOutput,
            "Continua.",
            3_500,
            3_900,
        ));

        assert_eq!(assembler.snapshot().turns.len(), 1);
    }

    #[test]
    fn speaker_change_ends_turn() {
        let mut assembler = assembler();
        assembler.ingest_segment(segment(AudioSource::SystemOutput, "Remoto.", 0, 500));
        let events =
            assembler.ingest_segment(segment(AudioSource::Microphone, "Usuário.", 700, 900));

        assert_eq!(finalized_turns(&events).len(), 1);
        assert_eq!(assembler.snapshot().turns.len(), 2);
    }

    #[test]
    fn source_change_ends_turn() {
        let mut assembler = assembler();
        assembler.ingest_segment(segment(AudioSource::Microphone, "Entrada.", 0, 500));
        let events =
            assembler.ingest_segment(segment(AudioSource::SystemOutput, "Saída.", 600, 900));

        assert_eq!(finalized_turns(&events).len(), 1);
        assert_eq!(assembler.snapshot().turns.len(), 2);
    }

    #[test]
    fn long_timeout_ends_turn() {
        let mut assembler = assembler();
        assembler.ingest_segment(segment(AudioSource::SystemOutput, "Primeira.", 0, 500));
        let events = assembler.ingest_segment(segment(
            AudioSource::SystemOutput,
            "Depois.",
            21_000,
            21_500,
        ));

        assert_eq!(finalized_turns(&events).len(), 1);
        assert_eq!(assembler.snapshot().turns.len(), 2);
    }

    #[test]
    fn long_timeout_is_configurable() {
        let mut assembler = ConversationAssembler::new(ConversationAssemblerConfig {
            turn_inactivity_timeout_ms: 5_000,
            ..ConversationAssemblerConfig::default()
        });
        assembler.ingest_segment(segment(AudioSource::SystemOutput, "Primeira.", 0, 500));
        assembler.ingest_segment(segment(AudioSource::SystemOutput, "Depois.", 6_000, 6_500));

        assert_eq!(assembler.snapshot().turns.len(), 2);
    }

    #[test]
    fn flush_ends_utterance_and_turn() {
        let mut assembler = assembler();
        assembler.ingest_segment(segment(AudioSource::Microphone, "Aberto.", 0, 500));
        let events = assembler.flush();

        assert_eq!(finalized_utterances(&events).len(), 1);
        assert_eq!(finalized_turns(&events).len(), 1);
    }

    #[test]
    fn pause_ends_utterance_and_turn_for_source() {
        let mut assembler = assembler();
        assembler.ingest_segment(segment(AudioSource::Microphone, "Aberto.", 0, 500));
        let events = assembler.finalize_source(AudioSource::Microphone, FinalizationReason::Paused);

        assert_eq!(finalized_utterances(&events).len(), 1);
        assert_eq!(finalized_turns(&events).len(), 1);
    }

    #[test]
    fn session_end_ends_utterance_and_turn() {
        let mut assembler = assembler();
        assembler.ingest_segment(segment(AudioSource::SystemOutput, "Aberto.", 0, 500));
        let events = assembler.end_session();

        assert_eq!(finalized_utterances(&events).len(), 1);
        assert_eq!(finalized_turns(&events).len(), 1);
    }

    #[test]
    fn defensive_maximum_turn_duration_creates_new_turn() {
        let mut assembler = ConversationAssembler::new(ConversationAssemblerConfig {
            maximum_turn_duration_ms: 1_000,
            ..ConversationAssemblerConfig::default()
        });
        assembler.ingest_segment(segment(AudioSource::Microphone, "Um.", 0, 700));
        let events =
            assembler.ingest_segment(segment(AudioSource::Microphone, "Dois.", 800, 1_200));

        assert_eq!(finalized_turns(&events).len(), 1);
        assert_eq!(assembler.snapshot().turns.len(), 2);
    }

    #[test]
    fn defensive_maximum_utterance_duration_creates_new_utterance_same_turn() {
        let mut assembler = ConversationAssembler::new(ConversationAssemblerConfig {
            maximum_utterance_duration_ms: 1_000,
            ..ConversationAssemblerConfig::default()
        });
        assembler.ingest_segment(segment(AudioSource::Microphone, "Um.", 0, 700));
        assembler.ingest_segment(segment(AudioSource::Microphone, "Dois.", 800, 1_200));

        let snapshot = assembler.snapshot();
        assert_eq!(snapshot.utterances.len(), 2);
        assert_eq!(snapshot.turns.len(), 1);
    }

    #[test]
    fn utterance_text_is_concatenated_without_rewriting() {
        let mut assembler = assembler();
        assembler.ingest_segment(segment(
            AudioSource::Microphone,
            "O Leandro   tem 21 anos.",
            0,
            500,
        ));
        assembler.ingest_segment(segment(
            AudioSource::Microphone,
            " No Grupo   Shop Mix...",
            600,
            900,
        ));

        assert_eq!(
            assembler.snapshot().utterances[0].text,
            "O Leandro tem 21 anos. No Grupo Shop Mix..."
        );
    }

    #[test]
    fn utterance_ids_are_preserved_in_turn() {
        let mut assembler = assembler();
        assembler.ingest_segment(segment(AudioSource::SystemOutput, "Primeira.", 0, 500));
        assembler.ingest_segment(segment(AudioSource::SystemOutput, "Segunda.", 3_000, 3_400));

        let snapshot = assembler.snapshot();
        assert_eq!(snapshot.turns[0].utterances.len(), 2);
        assert_eq!(snapshot.turns[0].utterances[0], snapshot.utterances[0].id);
        assert_eq!(snapshot.turns[0].utterances[1], snapshot.utterances[1].id);
    }

    #[test]
    fn segment_ids_remain_preserved_in_utterances() {
        let mut assembler = assembler();
        let first = segment(AudioSource::Microphone, "Um.", 0, 500);
        let second = segment(AudioSource::Microphone, "Dois.", 600, 900);
        let first_id = first.segment_id;
        let second_id = second.segment_id;
        assembler.ingest_segment(first);
        assembler.ingest_segment(second);

        assert_eq!(
            assembler.snapshot().utterances[0].segments,
            vec![first_id, second_id]
        );
    }

    #[test]
    fn speakers_are_never_mixed() {
        let mut assembler = assembler();
        assembler.ingest_segment(segment(AudioSource::Microphone, "Usuário.", 0, 500));
        assembler.ingest_segment(segment(AudioSource::SystemOutput, "Remoto.", 600, 900));

        let snapshot = assembler.snapshot();
        assert_eq!(snapshot.turns.len(), 2);
        assert_ne!(snapshot.turns[0].speaker, snapshot.turns[1].speaker);
    }

    #[test]
    fn events_keep_domain_order() {
        let mut assembler = assembler();
        let events = assembler.ingest_segment(segment(AudioSource::Microphone, "Um.", 0, 500));

        assert!(matches!(
            events[0],
            ConversationTimelineEvent::TurnStarted { .. }
        ));
        assert!(matches!(
            events[1],
            ConversationTimelineEvent::UtteranceStarted { .. }
        ));
        assert!(matches!(
            events[2],
            ConversationTimelineEvent::UtteranceUpdated { .. }
        ));
        assert!(matches!(
            events[3],
            ConversationTimelineEvent::TurnUpdated { .. }
        ));
    }

    #[test]
    fn serialization_uses_tauri_event_tags() {
        let mut assembler = assembler();
        let events = assembler.ingest_segment(segment(AudioSource::Microphone, "Um.", 0, 500));
        let json = serde_json::to_value(&events[0]).unwrap();

        assert_eq!(json["type"], "turn_started");
        assert_eq!(json["speaker"], "user");
    }

    #[test]
    fn out_of_order_segment_is_tolerated_for_open_utterance() {
        let mut assembler = assembler();
        assembler.ingest_segment(segment(AudioSource::Microphone, "Depois.", 1_000, 1_500));
        assembler.ingest_segment(segment(AudioSource::Microphone, "Antes.", 900, 1_100));

        assert_eq!(assembler.snapshot().utterances.len(), 1);
    }

    #[test]
    fn out_of_order_different_speaker_never_mixes() {
        let mut assembler = assembler();
        assembler.ingest_segment(segment(AudioSource::Microphone, "Usuário.", 1_000, 1_500));
        assembler.ingest_segment(segment(AudioSource::SystemOutput, "Remoto.", 900, 1_100));

        assert_eq!(assembler.snapshot().turns.len(), 2);
    }

    #[test]
    fn real_case_five_blocks_same_speaker_remain_one_turn() {
        let mut assembler = assembler();
        assembler.ingest_segment(segment(
            AudioSource::SystemOutput,
            "Explicação sobre apego evitativo...",
            0,
            22_000,
        ));
        assembler.ingest_segment(segment(
            AudioSource::SystemOutput,
            "Continuação da mesma explicação...",
            25_000,
            52_000,
        ));
        assembler.ingest_segment(segment(
            AudioSource::SystemOutput,
            "Continuação...",
            55_000,
            65_000,
        ));
        assembler.ingest_segment(segment(
            AudioSource::SystemOutput,
            "Continuação...",
            68_000,
            83_000,
        ));
        assembler.ingest_segment(segment(
            AudioSource::SystemOutput,
            "Continuação e uma pergunta?",
            86_000,
            87_000,
        ));

        let snapshot = assembler.snapshot();
        assert_eq!(snapshot.turns.len(), 1);
        assert_eq!(snapshot.utterances.len(), 5);
        assert!(snapshot.turns[0]
            .text
            .ends_with("Continuação e uma pergunta?"));
    }

    #[test]
    fn question_at_end_stays_inside_same_turn() {
        let mut assembler = assembler();
        assembler.ingest_segment(segment(
            AudioSource::SystemOutput,
            "Eu estava explicando.",
            0,
            1_000,
        ));
        assembler.ingest_segment(segment(
            AudioSource::SystemOutput,
            "Você concorda?",
            3_000,
            3_500,
        ));

        let snapshot = assembler.snapshot();
        assert_eq!(snapshot.turns.len(), 1);
        assert!(snapshot.turns[0].text.contains("Você concorda?"));
    }

    #[test]
    fn timeline_snapshot_keeps_turn_and_utterance_views_for_frontend() {
        let timeline = ConversationTimeline::default();
        timeline.ingest_transcript_event(TranscriptEvent::Ready(transcript(
            AudioSource::Microphone,
            "Um.",
            0,
            500,
        )));

        let snapshot = timeline.snapshot();
        assert_eq!(snapshot.turns.len(), 1);
        assert_eq!(snapshot.utterances.len(), 1);
        assert_eq!(
            snapshot.turns[0].utterances,
            vec![snapshot.utterances[0].id]
        );
    }

    // --- Timer dedicado da utterance ---------------------------------------------------
    //
    // Estes testes exercitam o timer real (`tokio::time::sleep` dentro de
    // `reschedule_utterance_timer`), mas sem nenhum sleep de parede: `start_paused = true`
    // congela o relógio do runtime de teste e `tokio::time::advance` avança o tempo
    // virtual instantaneamente. `yield_now` algumas vezes depois de `advance` dá ao
    // executor a chance de rodar a task que o `sleep` acordou antes do teste continuar.

    /// Fast-forwards virtual time past `gap_ms` and lets the spawned timer task run.
    /// `yield_now` first, *before* `advance`, matters: a task spawned just before this
    /// call hasn't been polled yet, so its `tokio::time::sleep` hasn't registered a
    /// deadline in the timer wheel — advancing time before that registration happens
    /// would fast-forward past nothing, and the task would only start sleeping (for the
    /// full duration, from the new "now") once it's finally polled. Yielding first lets
    /// it reach that first `.await` and register before we advance the clock.
    async fn advance_past_gap(gap_ms: u64) {
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(Duration::from_millis(gap_ms) + Duration::from_millis(1)).await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
    }

    fn finalized_utterance_events(
        events: &[ConversationTimelineEvent],
    ) -> Vec<&ConversationTimelineEvent> {
        events
            .iter()
            .filter(|e| matches!(e, ConversationTimelineEvent::UtteranceFinalized { .. }))
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn silence_alone_finalizes_utterance_without_a_new_segment() {
        let timeline = Arc::new(ConversationTimeline::default());
        timeline.attach();
        let sink_events: Arc<Mutex<Vec<ConversationTimelineEvent>>> =
            Arc::new(Mutex::new(Vec::new()));
        let sink_events_cb = sink_events.clone();
        timeline.set_event_sink(Arc::new(move |events| {
            sink_events_cb.lock().unwrap().extend(events);
        }));

        timeline.ingest_transcript_event(TranscriptEvent::Ready(transcript(
            AudioSource::SystemOutput,
            "Em qual situação você usaria microsserviços?",
            0,
            1_000,
        )));

        // Nada mais chega depois disso: sem novo segmento, sem flush, sem stop, sem fala
        // do usuário. Só o tempo passa.
        advance_past_gap(ConversationAssemblerConfig::default().same_speaker_utterance_gap_ms)
            .await;

        let events = sink_events.lock().unwrap();
        let finalized = finalized_utterance_events(&events);
        assert_eq!(
            finalized.len(),
            1,
            "the timer alone finalized the utterance"
        );
        let ConversationTimelineEvent::UtteranceFinalized {
            finalization_reason,
            ..
        } = finalized[0]
        else {
            unreachable!()
        };
        assert_eq!(
            *finalization_reason,
            UtteranceFinalizationReason::InactivityTimeout
        );
    }

    #[tokio::test(start_paused = true)]
    async fn utterance_timer_finalization_keeps_the_turn_open() {
        let timeline = Arc::new(ConversationTimeline::default());
        timeline.attach();
        let sink_events: Arc<Mutex<Vec<ConversationTimelineEvent>>> =
            Arc::new(Mutex::new(Vec::new()));
        let sink_events_cb = sink_events.clone();
        timeline.set_event_sink(Arc::new(move |events| {
            sink_events_cb.lock().unwrap().extend(events);
        }));

        timeline.ingest_transcript_event(TranscriptEvent::Ready(transcript(
            AudioSource::SystemOutput,
            "Pergunta.",
            0,
            1_000,
        )));
        advance_past_gap(ConversationAssemblerConfig::default().same_speaker_utterance_gap_ms)
            .await;

        let events = sink_events.lock().unwrap();
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, ConversationTimelineEvent::TurnFinalized { .. })),
            "only the utterance closes on the timer; the turn stays open for further grouping"
        );

        let snapshot = timeline.snapshot();
        assert_eq!(snapshot.turns.len(), 1);
        assert!(snapshot.turns[0].finalized_at.is_none());
        assert!(snapshot.utterances[0].finalized_at.is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn continuation_before_the_gap_prevents_early_finalization_and_the_stale_timer_is_a_noop()
    {
        let timeline = Arc::new(ConversationTimeline::default());
        timeline.attach();
        let sink_events: Arc<Mutex<Vec<ConversationTimelineEvent>>> =
            Arc::new(Mutex::new(Vec::new()));
        let sink_events_cb = sink_events.clone();
        timeline.set_event_sink(Arc::new(move |events| {
            sink_events_cb.lock().unwrap().extend(events);
        }));
        let gap_ms = ConversationAssemblerConfig::default().same_speaker_utterance_gap_ms;

        timeline.ingest_transcript_event(TranscriptEvent::Ready(transcript(
            AudioSource::SystemOutput,
            "Me conta um caso real",
            0,
            1_000,
        )));

        // Continuação chega bem antes do gap expirar: reagenda o timer (via nova
        // revisão), então o primeiro timer (agendado no ingest anterior) não deve
        // finalizar nada quando expirar.
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(Duration::from_millis(500)).await;
        timeline.ingest_transcript_event(TranscriptEvent::Ready(transcript(
            AudioSource::SystemOutput,
            "em que você optou por usar monolito.",
            1_500,
            2_500,
        )));

        // Passa o instante em que o timer *original* (agendado em t=0) teria expirado,
        // mas ainda não o suficiente para o timer *novo* (agendado em t=500ms).
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(Duration::from_millis(gap_ms - 500 + 1)).await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            finalized_utterance_events(&sink_events.lock().unwrap()).len(),
            0,
            "stale timer from before the continuation must not finalize early"
        );

        // Agora deixa o timer novo (reagendado na continuação) expirar de verdade.
        advance_past_gap(gap_ms).await;
        let events = sink_events.lock().unwrap();
        let finalized = finalized_utterance_events(&events);
        assert_eq!(
            finalized.len(),
            1,
            "exactly one finalization, from the rescheduled timer"
        );
        let ConversationTimelineEvent::UtteranceFinalized { utterance, .. } = finalized[0] else {
            unreachable!()
        };
        assert!(
            utterance.text.contains("Me conta") && utterance.text.contains("monolito"),
            "the finalized utterance contains the full, joined speech: {:?}",
            utterance.text
        );
    }

    #[tokio::test(start_paused = true)]
    async fn same_utterance_never_finalizes_twice_even_with_multiple_pending_timers() {
        let timeline = Arc::new(ConversationTimeline::default());
        timeline.attach();
        let sink_events: Arc<Mutex<Vec<ConversationTimelineEvent>>> =
            Arc::new(Mutex::new(Vec::new()));
        let sink_events_cb = sink_events.clone();
        timeline.set_event_sink(Arc::new(move |events| {
            sink_events_cb.lock().unwrap().extend(events);
        }));

        // Dois segmentos próximos no tempo agendam dois timers para a mesma utterance
        // (revisão 1 e revisão 2); só o timer da revisão mais recente deve produzir uma
        // finalização quando expirar.
        timeline.ingest_transcript_event(TranscriptEvent::Ready(transcript(
            AudioSource::SystemOutput,
            "Primeira parte.",
            0,
            500,
        )));
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(Duration::from_millis(100)).await;
        timeline.ingest_transcript_event(TranscriptEvent::Ready(transcript(
            AudioSource::SystemOutput,
            "Segunda parte.",
            600,
            900,
        )));

        advance_past_gap(ConversationAssemblerConfig::default().same_speaker_utterance_gap_ms)
            .await;

        let events = sink_events.lock().unwrap();
        assert_eq!(
            finalized_utterance_events(&events).len(),
            1,
            "only the timer matching the current revision finalizes; the stale one is a no-op"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn two_consecutive_remote_utterances_each_finalize_independently_via_timer() {
        let timeline = Arc::new(ConversationTimeline::default());
        timeline.attach();
        let sink_events: Arc<Mutex<Vec<ConversationTimelineEvent>>> =
            Arc::new(Mutex::new(Vec::new()));
        let sink_events_cb = sink_events.clone();
        timeline.set_event_sink(Arc::new(move |events| {
            sink_events_cb.lock().unwrap().extend(events);
        }));
        let gap_ms = ConversationAssemblerConfig::default().same_speaker_utterance_gap_ms;

        timeline.ingest_transcript_event(TranscriptEvent::Ready(transcript(
            AudioSource::SystemOutput,
            "Em qual situação você usaria monolitos?",
            0,
            1_000,
        )));
        advance_past_gap(gap_ms).await;
        assert_eq!(
            finalized_utterance_events(&sink_events.lock().unwrap()).len(),
            1
        );

        // Sem flush, sem stop — só mais silêncio e uma nova fala no mesmo turno (o gap já
        // estourou, então isso conta como uma nova utterance dentro do turno ainda aberto).
        timeline.ingest_transcript_event(TranscriptEvent::Ready(transcript(
            AudioSource::SystemOutput,
            "Em qual situação você usaria microsserviços?",
            gap_ms + 3_000,
            gap_ms + 4_000,
        )));
        advance_past_gap(gap_ms).await;

        let events = sink_events.lock().unwrap();
        assert_eq!(
            finalized_utterance_events(&events).len(),
            2,
            "each finalized utterance produced its own event, independently"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn stale_timer_after_manual_flush_is_a_noop() {
        let timeline = Arc::new(ConversationTimeline::default());
        timeline.attach();
        let sink_events: Arc<Mutex<Vec<ConversationTimelineEvent>>> =
            Arc::new(Mutex::new(Vec::new()));
        let sink_events_cb = sink_events.clone();
        timeline.set_event_sink(Arc::new(move |events| {
            sink_events_cb.lock().unwrap().extend(events);
        }));

        timeline.ingest_transcript_event(TranscriptEvent::Ready(transcript(
            AudioSource::SystemOutput,
            "Pergunta.",
            0,
            1_000,
        )));
        // Flush finaliza a utterance (e o turno) por fora do timer.
        let flush_events = timeline.flush();
        assert_eq!(finalized_utterance_events(&flush_events).len(), 1);

        // O timer que já estava agendado expira depois — não deve produzir uma segunda
        // finalização para a mesma utterance.
        advance_past_gap(ConversationAssemblerConfig::default().same_speaker_utterance_gap_ms)
            .await;
        let events = sink_events.lock().unwrap();
        assert_eq!(
            finalized_utterance_events(&events).len(),
            0,
            "the timer that fires after a manual flush must be a no-op, not a duplicate"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn set_utterance_gap_ms_changes_how_long_the_timer_waits() {
        let timeline = Arc::new(ConversationTimeline::default());
        timeline.attach();
        let sink_events: Arc<Mutex<Vec<ConversationTimelineEvent>>> =
            Arc::new(Mutex::new(Vec::new()));
        let sink_events_cb = sink_events.clone();
        timeline.set_event_sink(Arc::new(move |events| {
            sink_events_cb.lock().unwrap().extend(events);
        }));

        timeline.set_utterance_gap_ms(500);
        assert_eq!(timeline.utterance_gap_ms(), 500);

        timeline.ingest_transcript_event(TranscriptEvent::Ready(transcript(
            AudioSource::SystemOutput,
            "Pergunta rápida.",
            0,
            200,
        )));

        // Bem menos que o gap padrão de 1800ms, mas mais que o novo gap de 500ms.
        advance_past_gap(500).await;
        let events = sink_events.lock().unwrap();
        assert_eq!(
            finalized_utterance_events(&events).len(),
            1,
            "the shorter, dev-configured gap was honored by the timer"
        );
    }

    // --- Fronteira de sessão -----------------------------------------------------------

    #[test]
    fn ending_a_session_clears_every_conversational_collection() {
        let timeline = ConversationTimeline::default();
        timeline.ingest_transcript_event(TranscriptEvent::Ready(transcript(
            AudioSource::SystemOutput,
            "Em qual situação você escolheria usar monolitos?",
            0,
            1_000,
        )));
        assert!(!timeline.snapshot().turns.is_empty());

        let transition = timeline.end_session();
        assert_ne!(transition.previous_session_id, transition.session_id);

        let snapshot = timeline.snapshot();
        assert!(
            snapshot.turns.is_empty() && snapshot.utterances.is_empty(),
            "a sessão nova começa vazia: nada da anterior sobra no snapshot que o frontend relê"
        );
        assert!(
            timeline.raw_segments().is_empty(),
            "os segmentos brutos usados como contexto também são apagados"
        );
        assert_eq!(timeline.session_id(), transition.session_id);
    }

    #[test]
    fn ending_a_session_twice_is_idempotent() {
        let timeline = ConversationTimeline::default();
        timeline.ingest_transcript_event(TranscriptEvent::Ready(transcript(
            AudioSource::SystemOutput,
            "Pergunta.",
            0,
            1_000,
        )));

        let first = timeline.end_session();
        let second = timeline.end_session();

        assert_eq!(second.previous_session_id, first.session_id);
        assert_ne!(second.session_id, first.session_id);
        assert!(timeline.snapshot().turns.is_empty());
        // O segundo encerramento não tem nada aberto para finalizar.
        assert!(finalized_utterance_events(&second.events).is_empty());
    }

    #[test]
    fn session_events_mark_the_boundary_in_order() {
        let timeline = ConversationTimeline::default();
        let transition = timeline.end_session();
        let kinds: Vec<&str> = transition
            .events
            .iter()
            .map(|e| match e {
                ConversationTimelineEvent::SessionEnded { .. } => "session_ended",
                ConversationTimelineEvent::SessionStarted { .. } => "session_started",
                _ => "other",
            })
            .collect();
        let ended = kinds.iter().position(|k| *k == "session_ended").unwrap();
        let started = kinds.iter().position(|k| *k == "session_started").unwrap();
        assert!(ended < started);
    }

    #[test]
    fn finalized_events_carry_the_session_that_owned_them() {
        let timeline = ConversationTimeline::default();
        let session = timeline.session_id();
        timeline.ingest_transcript_event(TranscriptEvent::Ready(transcript(
            AudioSource::SystemOutput,
            "Pergunta.",
            0,
            1_000,
        )));
        let events = timeline.end_session().events;

        let utterance_sessions: Vec<SessionId> = events
            .iter()
            .filter_map(|e| match e {
                ConversationTimelineEvent::UtteranceFinalized { session_id, .. } => {
                    Some(*session_id)
                }
                _ => None,
            })
            .collect();
        assert_eq!(utterance_sessions, vec![session]);
        let turn_sessions: Vec<SessionId> = events
            .iter()
            .filter_map(|e| match e {
                ConversationTimelineEvent::TurnFinalized { session_id, .. } => Some(*session_id),
                _ => None,
            })
            .collect();
        assert_eq!(turn_sessions, vec![session]);
    }

    #[test]
    fn utterances_finalized_at_session_end_are_marked_as_such() {
        let timeline = ConversationTimeline::default();
        timeline.ingest_transcript_event(TranscriptEvent::Ready(transcript(
            AudioSource::SystemOutput,
            "Pergunta que ficou aberta.",
            0,
            1_000,
        )));
        let events = timeline.end_session().events;
        let finalized = finalized_utterance_events(&events);
        assert_eq!(finalized.len(), 1);
        let ConversationTimelineEvent::UtteranceFinalized {
            finalization_reason,
            ..
        } = finalized[0]
        else {
            unreachable!()
        };
        assert_eq!(
            *finalization_reason,
            UtteranceFinalizationReason::SessionEnded,
            "é este motivo que faz o ResponseEngine recusar o gatilho — ver \
             response_provider::engine::triggers_generation"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_timer_from_the_previous_session_cannot_finalize_anything_in_the_new_one() {
        let timeline = Arc::new(ConversationTimeline::default());
        timeline.attach();
        let sink_events: Arc<Mutex<Vec<ConversationTimelineEvent>>> =
            Arc::new(Mutex::new(Vec::new()));
        let sink_events_cb = sink_events.clone();
        timeline.set_event_sink(Arc::new(move |events| {
            sink_events_cb.lock().unwrap().extend(events);
        }));

        timeline.ingest_transcript_event(TranscriptEvent::Ready(transcript(
            AudioSource::SystemOutput,
            "Fala da sessão A.",
            0,
            1_000,
        )));

        // Encerra a sessão antes de o timer expirar: o token da sessão é cancelado e o
        // timer agendado morre com ela.
        timeline.end_session();
        sink_events.lock().unwrap().clear();

        advance_past_gap(ConversationAssemblerConfig::default().same_speaker_utterance_gap_ms)
            .await;

        let events = sink_events.lock().unwrap();
        assert!(
            finalized_utterance_events(&events).is_empty(),
            "um timer da sessão A não pode finalizar nada depois que a sessão B começou: {:?}",
            events.len()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_new_session_has_its_own_working_timer() {
        let timeline = Arc::new(ConversationTimeline::default());
        timeline.attach();
        let sink_events: Arc<Mutex<Vec<ConversationTimelineEvent>>> =
            Arc::new(Mutex::new(Vec::new()));
        let sink_events_cb = sink_events.clone();
        timeline.set_event_sink(Arc::new(move |events| {
            sink_events_cb.lock().unwrap().extend(events);
        }));

        timeline.end_session();
        sink_events.lock().unwrap().clear();

        timeline.ingest_transcript_event(TranscriptEvent::Ready(transcript(
            AudioSource::SystemOutput,
            "Fala da sessão B.",
            0,
            1_000,
        )));
        advance_past_gap(ConversationAssemblerConfig::default().same_speaker_utterance_gap_ms)
            .await;

        let events = sink_events.lock().unwrap();
        assert_eq!(
            finalized_utterance_events(&events).len(),
            1,
            "cancelar a sessão anterior não pode desligar o timer da sessão nova"
        );
    }

    /// Replica exatamente a sequência que `AppRouter::startSession` produz: abrir a
    /// fronteira de sessão e só então começar a receber áudio. A tela de sessão decide
    /// entre "Ouvindo a conversa..." e a sugestão a partir de existir ou não um turno
    /// elegível (`other_person` + `system_output`), então se esta sequência não produzir
    /// `TurnStarted` a janela fica presa no estado de espera para sempre.
    #[tokio::test]
    async fn starting_a_session_then_capturing_still_produces_an_eligible_turn() {
        let timeline = Arc::new(ConversationTimeline::default());
        timeline.attach();

        timeline.start_session();

        let events = timeline.ingest_transcript_event(TranscriptEvent::Ready(transcript(
            AudioSource::SystemOutput,
            "Em qual situação você usaria monolitos?",
            0,
            1_500,
        )));

        let started = events.iter().any(|event| {
            matches!(
                event,
                ConversationTimelineEvent::TurnStarted {
                    speaker: ConversationSpeaker::OtherPerson,
                    source: AudioSource::SystemOutput,
                    ..
                }
            )
        });
        assert!(
            started,
            "a primeira fala da outra pessoa depois de start_session tem que abrir um turno \
             elegível: {events:?}"
        );

        let snapshot = timeline.snapshot();
        assert!(
            snapshot.turns.iter().any(|turn| {
                turn.speaker == ConversationSpeaker::OtherPerson
                    && turn.source == AudioSource::SystemOutput
            }),
            "o snapshot que a janela de sessão lê ao montar precisa conter o turno elegível"
        );
    }
}
