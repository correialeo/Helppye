//! Registro de traces vivos e concluídos, com correlação pelos ids que o pipeline já
//! produz.
//!
//! O problema que ele resolve: nenhum id acompanha uma fala do começo ao fim. O áudio
//! conhece `SegmentId`, a timeline conhece `UtteranceId`, o motor de resposta conhece
//! `GenerationId`, e cada camada só sabe o id da anterior no momento em que recebe algo
//! dela. O recorder guarda um trace por fala e três mapas de correlação, populados no
//! instante exato em que cada camada aprende o id seguinte — assim `mark_utterance(...)`
//! encontra o trace que começou lá atrás no primeiro chunk de áudio.
//!
//! Tudo é limitado: `MAX_LIVE_TRACES` traces vivos e `MAX_COMPLETED_TRACES` concluídos, em
//! anel. Uma reunião de duas horas não pode virar um vazamento de memória por telemetria.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::Instant;

use serde::Serialize;

use crate::audio::segment::SegmentId;
use crate::audio::types::AudioSource;
use crate::conversation::{SessionId, UtteranceId};
use crate::telemetry::{
    ContentPolicy, Milestone, PipelineTrace, PipelineTraceSnapshot, ProviderTelemetryEvent,
    TraceAttributes,
};

/// Traces simultaneamente abertos. Na prática há no máximo um por fonte de áudio; a folga
/// existe porque uma fala pode ficar aberta enquanto a seguinte já começou na outra fonte.
const MAX_LIVE_TRACES: usize = 64;
/// Traces concluídos retidos para inspeção (modo de desenvolvedor, harness de benchmark).
const MAX_COMPLETED_TRACES: usize = 256;

/// Id de trace, monotônico por processo. Não é derivado de nenhum id de domínio de
/// propósito: um trace pode começar antes de existir segmento, utterance ou geração.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct TraceId(pub u64);

impl std::fmt::Display for TraceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Default)]
struct RecorderState {
    next_id: u64,
    live: HashMap<TraceId, PipelineTrace>,
    live_order: VecDeque<TraceId>,
    by_source: HashMap<(SessionId, AudioSource), TraceId>,
    by_segment: HashMap<SegmentId, TraceId>,
    by_utterance: HashMap<UtteranceId, TraceId>,
    by_generation: HashMap<u64, TraceId>,
    completed: VecDeque<PipelineTraceSnapshot>,
}

impl RecorderState {
    fn evict_if_needed(&mut self) {
        while self.live_order.len() > MAX_LIVE_TRACES {
            if let Some(oldest) = self.live_order.pop_front() {
                self.drop_trace(oldest);
            }
        }
    }

    fn drop_trace(&mut self, id: TraceId) -> Option<PipelineTrace> {
        let trace = self.live.remove(&id)?;
        self.by_source.retain(|_, v| *v != id);
        self.by_segment.retain(|_, v| *v != id);
        self.by_utterance.retain(|_, v| *v != id);
        self.by_generation.retain(|_, v| *v != id);
        self.live_order.retain(|v| *v != id);
        Some(trace)
    }
}

/// Ponto único de gravação. Todos os métodos aceitam `&self` e nunca devolvem erro: uma
/// falha de telemetria jamais pode interromper a transcrição ou a geração. Um marco cujo
/// trace não existe mais (evictado, ou de uma sessão encerrada) é simplesmente ignorado.
pub struct TelemetryRecorder {
    state: Mutex<RecorderState>,
    content_policy: Mutex<ContentPolicy>,
}

impl Default for TelemetryRecorder {
    fn default() -> Self {
        TelemetryRecorder {
            state: Mutex::new(RecorderState::default()),
            content_policy: Mutex::new(ContentPolicy::default()),
        }
    }
}

impl TelemetryRecorder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn content_policy(&self) -> ContentPolicy {
        *self.content_policy.lock().unwrap()
    }

    /// Ligado ao "Modo de desenvolvedor". Fora dele, nenhum texto de reunião é retido.
    pub fn set_content_policy(&self, policy: ContentPolicy) {
        *self.content_policy.lock().unwrap() = policy;
    }

    /// Abre (ou reaproveita) o trace da fala corrente daquela fonte. Reaproveitar é o
    /// comportamento correto: vários chunks de áudio da mesma fala precisam cair no mesmo
    /// trace, e quem os empurra não tem como saber se é o primeiro.
    pub fn begin_or_current(&self, session_id: SessionId, source: AudioSource) -> TraceId {
        self.begin_or_current_at(session_id, source, Instant::now())
    }

    pub fn begin_or_current_at(
        &self,
        session_id: SessionId,
        source: AudioSource,
        origin: Instant,
    ) -> TraceId {
        let mut state = self.state.lock().unwrap();
        if let Some(id) = state.by_source.get(&(session_id, source)) {
            return *id;
        }
        state.next_id += 1;
        let id = TraceId(state.next_id);
        state
            .live
            .insert(id, PipelineTrace::new_at(id, session_id, source, origin));
        state.live_order.push_back(id);
        state.by_source.insert((session_id, source), id);
        state.evict_if_needed();
        id
    }

    pub fn mark(&self, id: TraceId, milestone: Milestone) {
        let mut state = self.state.lock().unwrap();
        if let Some(trace) = state.live.get_mut(&id) {
            trace.mark(milestone);
        }
    }

    /// Marco cujo instante foi capturado antes — ver `PipelineTrace::mark_at`.
    pub fn mark_at(&self, id: TraceId, milestone: Milestone, at: std::time::Instant) {
        let mut state = self.state.lock().unwrap();
        if let Some(trace) = state.live.get_mut(&id) {
            trace.mark_at(milestone, at);
        }
    }

    /// Correlaciona o trace com o segmento de áudio que a transcrição vai devolver depois.
    pub fn link_segment(&self, id: TraceId, segment_id: SegmentId) {
        let mut state = self.state.lock().unwrap();
        if state.live.contains_key(&id) {
            state.by_segment.insert(segment_id, id);
        }
    }

    pub fn link_utterance(&self, id: TraceId, utterance_id: UtteranceId) {
        let mut state = self.state.lock().unwrap();
        if state.live.contains_key(&id) {
            state.by_utterance.insert(utterance_id, id);
        }
    }

    pub fn link_generation(&self, id: TraceId, generation_id: u64) {
        let mut state = self.state.lock().unwrap();
        if state.live.contains_key(&id) {
            state.by_generation.insert(generation_id, id);
        }
    }

    pub fn trace_for_segment(&self, segment_id: SegmentId) -> Option<TraceId> {
        self.state
            .lock()
            .unwrap()
            .by_segment
            .get(&segment_id)
            .copied()
    }

    pub fn trace_for_utterance(&self, utterance_id: UtteranceId) -> Option<TraceId> {
        self.state
            .lock()
            .unwrap()
            .by_utterance
            .get(&utterance_id)
            .copied()
    }

    pub fn trace_for_generation(&self, generation_id: u64) -> Option<TraceId> {
        self.state
            .lock()
            .unwrap()
            .by_generation
            .get(&generation_id)
            .copied()
    }

    /// Muda atributos do trace. `None` em qualquer campo significa "não sei ainda", nunca
    /// "apague o que já sabia": um `update` posterior só sobrescreve o que traz preenchido.
    pub fn record_attributes(&self, id: TraceId, update: TraceAttributes) {
        let mut state = self.state.lock().unwrap();
        let Some(trace) = state.live.get_mut(&id) else {
            return;
        };
        let attributes = trace.attributes_mut();
        merge(
            &mut attributes.transcription_queue_wait_ms,
            update.transcription_queue_wait_ms,
        );
        merge(
            &mut attributes.provider_queue_wait_ms,
            update.provider_queue_wait_ms,
        );
        merge(
            &mut attributes.provider_queue_depth,
            update.provider_queue_depth,
        );
        merge(
            &mut attributes.provider_queue_oldest_age_ms,
            update.provider_queue_oldest_age_ms,
        );
        merge(
            &mut attributes.dropped_audio_chunks,
            update.dropped_audio_chunks,
        );
        merge(
            &mut attributes.audio_chunk_duration_ms,
            update.audio_chunk_duration_ms,
        );
        merge(&mut attributes.audio_chunks_sent, update.audio_chunks_sent);
        merge(&mut attributes.bytes_sent, update.bytes_sent);
        merge(
            &mut attributes.websocket_send_latency_ms,
            update.websocket_send_latency_ms,
        );
        merge(
            &mut attributes.automatic_vad_enabled,
            update.automatic_vad_enabled,
        );
        merge(
            &mut attributes.finalization_strategy,
            update.finalization_strategy,
        );
        merge(
            &mut attributes.finalization_reason,
            update.finalization_reason,
        );
        merge(
            &mut attributes.partial_revision_count,
            update.partial_revision_count,
        );
        merge(
            &mut attributes.transcription_provider,
            update.transcription_provider,
        );
        merge(
            &mut attributes.transcription_model,
            update.transcription_model,
        );
        merge(&mut attributes.response_provider, update.response_provider);
        merge(&mut attributes.response_model, update.response_model);
        merge(&mut attributes.raw_text_length, update.raw_text_length);
        merge(
            &mut attributes.normalized_text_length,
            update.normalized_text_length,
        );
        merge(
            &mut attributes.normalization_change_count,
            update.normalization_change_count,
        );
        merge(
            &mut attributes.context_turn_count,
            update.context_turn_count,
        );
        merge(
            &mut attributes.context_character_count,
            update.context_character_count,
        );
        merge(&mut attributes.sanitized_text, update.sanitized_text);
    }

    pub fn record_provider_event(&self, id: TraceId, event: ProviderTelemetryEvent) {
        let mut state = self.state.lock().unwrap();
        let Some(trace) = state.live.get_mut(&id) else {
            return;
        };
        match event {
            ProviderTelemetryEvent::Configuration {
                automatic_vad_enabled,
                finalization_strategy,
            } => {
                trace.attributes_mut().automatic_vad_enabled = Some(automatic_vad_enabled);
                trace.attributes_mut().finalization_strategy = Some(finalization_strategy);
            }
            ProviderTelemetryEvent::AudioChunkSent {
                duration_ms,
                bytes,
                send_duration_ms,
            } => {
                trace.mark(Milestone::FirstAudioChunkSent);
                trace.mark(Milestone::LastAudioChunkSent);
                let attributes = trace.attributes_mut();
                attributes.audio_chunk_duration_ms = Some(duration_ms);
                attributes.audio_chunks_sent = Some(attributes.audio_chunks_sent.unwrap_or(0) + 1);
                attributes.bytes_sent = Some(attributes.bytes_sent.unwrap_or(0) + bytes);
                attributes.websocket_send_latency_ms = Some(send_duration_ms);
            }
            ProviderTelemetryEvent::ActivityStartSent => {
                trace.mark(Milestone::ActivityStartSent);
            }
            ProviderTelemetryEvent::ActivityEndSent => {
                trace.mark(Milestone::ActivityEndSent);
            }
            ProviderTelemetryEvent::InputTranscriptionReceived => {
                trace.mark(Milestone::FirstInputTranscriptionReceived);
                trace.mark(Milestone::LastInputTranscriptionReceived);
                let attributes = trace.attributes_mut();
                attributes.partial_revision_count =
                    Some(attributes.partial_revision_count.unwrap_or(0) + 1);
            }
            ProviderTelemetryEvent::ServerTurnCompleteReceived => {
                trace.mark(Milestone::ServerTurnCompleteReceived);
            }
            ProviderTelemetryEvent::LocalFinalTranscriptEmitted {
                finalization_reason,
            } => {
                trace.mark(Milestone::LocalFinalTranscriptEmitted);
                trace.attributes_mut().finalization_reason = Some(finalization_reason);
            }
        }
    }

    /// Grava texto **apenas** se a política vigente permitir, já sanitizado. Chamar isto no
    /// caminho normal é seguro: sob `Redacted` é um no-op.
    pub fn record_text(&self, id: TraceId, text: &str) {
        let Some(sanitized) = self.content_policy().sanitize(text) else {
            return;
        };
        let mut state = self.state.lock().unwrap();
        if let Some(trace) = state.live.get_mut(&id) {
            trace.attributes_mut().sanitized_text = Some(sanitized);
        }
    }

    /// Fecha o trace, move o snapshot para o anel de concluídos e libera as correlações.
    /// Devolve o snapshot para quem quiser logar/emitir na hora.
    pub fn finish(&self, id: TraceId) -> Option<PipelineTraceSnapshot> {
        let mut state = self.state.lock().unwrap();
        let trace = state.drop_trace(id)?;
        let snapshot = trace.snapshot();
        state.completed.push_back(snapshot.clone());
        while state.completed.len() > MAX_COMPLETED_TRACES {
            state.completed.pop_front();
        }
        Some(snapshot)
    }

    /// Descarta todo trace vivo de uma sessão que acabou. Diferente de `finish`: o resultado
    /// não é publicado, porque uma fala interrompida pelo fim da sessão não tem latência de
    /// ponta a ponta para reportar.
    pub fn discard_session(&self, session_id: SessionId) {
        let mut state = self.state.lock().unwrap();
        let ids: Vec<TraceId> = state
            .live
            .values()
            .filter(|t| t.session_id() == session_id)
            .map(|t| t.id())
            .collect();
        for id in ids {
            state.drop_trace(id);
        }
    }

    pub fn snapshot(&self, id: TraceId) -> Option<PipelineTraceSnapshot> {
        self.state
            .lock()
            .unwrap()
            .live
            .get(&id)
            .map(|t| t.snapshot())
    }

    /// Últimos traces concluídos, do mais recente para o mais antigo.
    pub fn recent(&self, limit: usize) -> Vec<PipelineTraceSnapshot> {
        self.state
            .lock()
            .unwrap()
            .completed
            .iter()
            .rev()
            .take(limit)
            .cloned()
            .collect()
    }

    pub fn live_count(&self) -> usize {
        self.state.lock().unwrap().live.len()
    }

    pub fn completed_count(&self) -> usize {
        self.state.lock().unwrap().completed.len()
    }
}

fn merge<T>(slot: &mut Option<T>, update: Option<T>) {
    if let Some(value) = update {
        *slot = Some(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recorder() -> TelemetryRecorder {
        TelemetryRecorder::new()
    }

    #[test]
    fn same_source_and_session_reuse_the_same_trace() {
        let r = recorder();
        let session = SessionId::from_value(1);
        let a = r.begin_or_current(session, AudioSource::SystemOutput);
        let b = r.begin_or_current(session, AudioSource::SystemOutput);
        assert_eq!(a, b);
        assert_eq!(r.live_count(), 1);
    }

    #[test]
    fn microphone_and_system_output_never_share_a_trace() {
        let r = recorder();
        let session = SessionId::from_value(1);
        let mic = r.begin_or_current(session, AudioSource::Microphone);
        let system = r.begin_or_current(session, AudioSource::SystemOutput);
        assert_ne!(mic, system);
        assert_eq!(r.live_count(), 2);
    }

    #[test]
    fn correlation_walks_from_segment_to_utterance_to_generation() {
        let r = recorder();
        let session = SessionId::from_value(1);
        let trace = r.begin_or_current(session, AudioSource::SystemOutput);
        let segment = SegmentId::next();
        r.link_segment(trace, segment);
        assert_eq!(r.trace_for_segment(segment), Some(trace));

        let utterance = UtteranceId::from_raw(7);
        r.link_utterance(trace, utterance);
        assert_eq!(r.trace_for_utterance(utterance), Some(trace));

        r.link_generation(trace, 42);
        assert_eq!(r.trace_for_generation(42), Some(trace));
    }

    #[test]
    fn marking_an_unknown_trace_is_a_no_op() {
        let r = recorder();
        r.mark(TraceId(999), Milestone::FinalTranscript);
        assert_eq!(r.live_count(), 0);
        assert_eq!(r.completed_count(), 0);
    }

    #[test]
    fn finish_moves_the_trace_to_completed_and_frees_correlations() {
        let r = recorder();
        let session = SessionId::from_value(1);
        let trace = r.begin_or_current(session, AudioSource::SystemOutput);
        let segment = SegmentId::next();
        r.link_segment(trace, segment);
        r.mark(trace, Milestone::SpeechEnded);
        r.mark(trace, Milestone::FinalTranscript);

        let snapshot = r.finish(trace).expect("trace concluído");
        assert_eq!(snapshot.session_id, session);
        assert_eq!(r.live_count(), 0);
        assert_eq!(r.completed_count(), 1);
        assert_eq!(r.trace_for_segment(segment), None);
        // Um novo trace da mesma fonte é realmente novo, não o anterior reaproveitado.
        assert_ne!(
            r.begin_or_current(session, AudioSource::SystemOutput),
            trace
        );
    }

    #[test]
    fn ending_a_session_discards_its_live_traces_without_publishing_them() {
        let r = recorder();
        let ending = SessionId::from_value(1);
        let next = SessionId::from_value(2);
        r.begin_or_current(ending, AudioSource::SystemOutput);
        r.begin_or_current(next, AudioSource::SystemOutput);

        r.discard_session(ending);
        assert_eq!(r.live_count(), 1);
        assert_eq!(
            r.completed_count(),
            0,
            "fala interrompida pelo fim da sessão não vira métrica de ponta a ponta"
        );
    }

    #[test]
    fn content_is_not_recorded_outside_developer_mode() {
        let r = recorder();
        let trace = r.begin_or_current(SessionId::from_value(1), AudioSource::SystemOutput);
        r.record_text(trace, "conteúdo confidencial da reunião");
        assert_eq!(r.snapshot(trace).unwrap().attributes.sanitized_text, None);

        r.set_content_policy(ContentPolicy::Developer);
        r.record_text(trace, "conteúdo confidencial da reunião");
        assert_eq!(
            r.snapshot(trace)
                .unwrap()
                .attributes
                .sanitized_text
                .as_deref(),
            Some("conteúdo confidencial da reunião")
        );
    }

    #[test]
    fn attributes_merge_instead_of_overwriting_with_none() {
        let r = recorder();
        let trace = r.begin_or_current(SessionId::from_value(1), AudioSource::SystemOutput);
        r.record_attributes(
            trace,
            TraceAttributes {
                transcription_provider: Some("whisper_local".into()),
                raw_text_length: Some(10),
                ..Default::default()
            },
        );
        r.record_attributes(
            trace,
            TraceAttributes {
                response_provider: Some("ollama".into()),
                ..Default::default()
            },
        );

        let attributes = r.snapshot(trace).unwrap().attributes;
        assert_eq!(
            attributes.transcription_provider.as_deref(),
            Some("whisper_local")
        );
        assert_eq!(attributes.raw_text_length, Some(10));
        assert_eq!(attributes.response_provider.as_deref(), Some("ollama"));
    }

    #[test]
    fn live_traces_are_bounded() {
        let r = recorder();
        for i in 0..(MAX_LIVE_TRACES as u64 + 10) {
            r.begin_or_current(SessionId::from_value(i), AudioSource::SystemOutput);
        }
        assert!(r.live_count() <= MAX_LIVE_TRACES);
    }
}
