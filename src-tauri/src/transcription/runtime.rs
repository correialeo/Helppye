//! Ciclo de vida e isolamento da camada de transcrição.
//!
//! Uma sessão de conversa possui **suas próprias** sessões de transcrição: uma por fonte
//! (microfone, saída de sistema). O runtime é quem as abre, roteia áudio para a certa,
//! encerra todas na fronteira de sessão e — o ponto central — **descarta no backend** todo
//! evento que não pertence ao estado atual.
//!
//! Por que o descarte precisa acontecer aqui e não no frontend: entre o provider e a tela
//! existe a Conversation Timeline e o `ResponseEngine`. Um resultado atrasado que chegasse
//! à timeline já teria virado segmento, aberto utterance, disparado geração e consumido o
//! modelo — o frontend descartaria o evento *depois* de tudo isso ter acontecido. O filtro
//! só é eficaz antes da timeline.
//!
//! Três chaves compõem a decisão, e cada uma cobre um caso que as outras não cobrem:
//!
//! - `session_id` — o resultado é de uma sessão de conversa anterior.
//! - `transcription_session_id` — mesma sessão de conversa, mas de uma sessão de
//!   transcrição já substituída (troca de provider ou de dispositivo no meio da sessão).
//! - `provider_event_id` — o mesmo resultado entregue duas vezes (retry de rede, reentrega
//!   de stream). Sem isso a fala apareceria duplicada na timeline.
//!
//! Ordem do encerramento (`end_session`), que é o que torna o isolamento real: bloquear
//! chunks novos → invalidar a identidade de sessão (todo evento em voo já é descartado a
//! partir daqui) → cancelar os providers → limpar buffers. Cancelar antes de bloquear
//! deixaria uma janela em que um chunk novo reabriria uma sessão que acabou de ser
//! cancelada.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use serde::Serialize;
use tokio::sync::Mutex as AsyncMutex;
use tracing::{debug, warn};

use crate::audio::segment::AudioSegment;
use crate::audio::types::AudioSource;
use crate::conversation::SessionId;
use crate::normalization::{
    ContextualCorrectionInput, ContextualCorrector, DeterministicNormalizer,
    TranscriptCorrectionMode, TranscriptNormalizationInput, TranscriptNormalizationResult,
    TranscriptNormalizer,
};
use crate::telemetry::{Milestone, TelemetryRecorder, TraceAttributes};
use crate::transcription::error::TranscriptionError;
use crate::transcription::events::{FinalTranscript, ProviderEventId, TranscriptionEvent};
use crate::transcription::provider::TranscriptionProvider;
use crate::transcription::session::{
    AudioChunk, TranscriptionSession, TranscriptionSessionContext, TranscriptionSessionId,
};
use crate::transcription::settings::TranscriptionSettings;

/// Quantos `provider_event_id` recentes cada sessão de transcrição lembra. Limitado porque
/// uma sessão longa produziria um conjunto sem fim; 512 cobre com folga qualquer reentrega
/// plausível (que acontece em segundos, não em horas de reunião).
const DEDUPE_WINDOW: usize = 512;

/// Quantas falas recentes da sessão ficam disponíveis como contexto para um corretor
/// contextual. Pequeno de propósito: é contexto de *correção* (o termo técnico que apareceu
/// há dez segundos), não histórico de conversa — esse é responsabilidade do
/// `ResponseContextBuilder`.
const CORRECTION_CONTEXT_UTTERANCES: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscardReason {
    /// Nenhuma sessão de conversa ativa (entre `end_session` e o próximo `begin_session`).
    NoActiveSession,
    /// Evento de uma sessão de conversa anterior.
    StaleSession,
    /// Sessão de conversa correta, sessão de transcrição já substituída.
    StaleTranscriptionSession,
    /// `provider_event_id` já visto nesta sessão de transcrição.
    DuplicateEvent,
}

/// Resultado final aceito, já normalizado. O texto bruto viaja junto: diagnóstico usa o
/// bruto, prompt usa o normalizado, e nenhum dos dois some.
#[derive(Debug, Clone)]
pub struct NormalizedTranscript {
    pub transcript: FinalTranscript,
    pub normalization: TranscriptNormalizationResult,
}

#[derive(Debug, Clone)]
pub enum TranscriptionRuntimeOutput {
    /// Evento validado, para observadores (emissão ao frontend, telemetria). Inclui
    /// parciais e fronteiras de fala — nada disso vira segmento.
    Event(TranscriptionEvent),
    /// Resultado final aceito e normalizado: o único que deve virar `TranscriptSegment`.
    Final(Box<NormalizedTranscript>),
    /// Evento recusado, com o motivo. Publicado para que o descarte seja **visível** em
    /// diagnóstico em vez de silencioso.
    Discarded {
        reason: DiscardReason,
        session_id: SessionId,
        transcription_session_id: TranscriptionSessionId,
        source: AudioSource,
    },
}

pub type TranscriptionOutputSink = Arc<dyn Fn(TranscriptionRuntimeOutput) + Send + Sync>;

#[derive(Debug, Default)]
struct RuntimeCounters {
    accepted_finals: AtomicU64,
    discarded_no_session: AtomicU64,
    discarded_stale_session: AtomicU64,
    discarded_stale_transcription_session: AtomicU64,
    discarded_duplicate: AtomicU64,
    push_errors: AtomicU64,
}

/// Snapshot legível dos contadores, para o painel de modo de desenvolvedor.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct TranscriptionRuntimeStats {
    pub accepted_finals: u64,
    pub discarded_no_session: u64,
    pub discarded_stale_session: u64,
    pub discarded_stale_transcription_session: u64,
    pub discarded_duplicate: u64,
    pub push_errors: u64,
}

/// Estado consultado de forma **síncrona** por cada evento que chega de um provider. Fica
/// num `std::sync::Mutex` separado do mapa de sessões (que é `async`) de propósito: o filtro
/// precisa decidir sem `await`, senão um evento obsoleto poderia atravessar a fronteira
/// enquanto espera o lock assíncrono.
#[derive(Default)]
struct Gate {
    session_id: Option<SessionId>,
    active: HashMap<AudioSource, TranscriptionSessionId>,
    seen: HashMap<TranscriptionSessionId, (HashSet<ProviderEventId>, VecDeque<ProviderEventId>)>,
}

impl Gate {
    fn accept(&mut self, event: &TranscriptionEvent) -> Result<(), DiscardReason> {
        let Some(current) = self.session_id else {
            return Err(DiscardReason::NoActiveSession);
        };
        if event.session_id() != current {
            return Err(DiscardReason::StaleSession);
        }
        let transcription_session_id = event.transcription_session_id();
        match self.active.get(&event.source()) {
            Some(active) if *active == transcription_session_id => {}
            _ => return Err(DiscardReason::StaleTranscriptionSession),
        }
        if let Some(event_id) = event.provider_event_id() {
            let (set, order) = self.seen.entry(transcription_session_id).or_default();
            if !set.insert(event_id.clone()) {
                return Err(DiscardReason::DuplicateEvent);
            }
            order.push_back(event_id.clone());
            while order.len() > DEDUPE_WINDOW {
                if let Some(evicted) = order.pop_front() {
                    set.remove(&evicted);
                }
            }
        }
        Ok(())
    }
}

struct ActiveSession {
    id: TranscriptionSessionId,
    session: Box<dyn TranscriptionSession>,
}

pub struct TranscriptionRuntime {
    provider: StdMutex<Arc<dyn TranscriptionProvider>>,
    settings: StdMutex<TranscriptionSettings>,
    normalizer: StdMutex<Arc<dyn TranscriptNormalizer>>,
    correction_mode: StdMutex<TranscriptCorrectionMode>,
    /// `None` em toda build atual — ver `normalization::correction`. O campo existe para que
    /// a extensão seja um registro, não uma refatoração.
    corrector: StdMutex<Option<Arc<dyn ContextualCorrector>>>,
    recent_texts: Arc<StdMutex<VecDeque<String>>>,
    gate: Arc<StdMutex<Gate>>,
    sessions: AsyncMutex<HashMap<AudioSource, ActiveSession>>,
    sink: TranscriptionOutputSink,
    counters: Arc<RuntimeCounters>,
    telemetry: Arc<TelemetryRecorder>,
}

impl TranscriptionRuntime {
    pub fn new(
        provider: Arc<dyn TranscriptionProvider>,
        settings: TranscriptionSettings,
        sink: TranscriptionOutputSink,
    ) -> Self {
        Self::with_telemetry(
            provider,
            settings,
            sink,
            Arc::clone(crate::telemetry::recorder()),
        )
    }

    /// Usado nos testes para observar os marcos gravados sem depender do recorder do
    /// processo (que outros testes rodando em paralelo também tocariam).
    pub fn with_telemetry(
        provider: Arc<dyn TranscriptionProvider>,
        settings: TranscriptionSettings,
        sink: TranscriptionOutputSink,
        telemetry: Arc<TelemetryRecorder>,
    ) -> Self {
        TranscriptionRuntime {
            provider: StdMutex::new(provider),
            settings: StdMutex::new(settings),
            normalizer: StdMutex::new(Arc::new(DeterministicNormalizer::default())),
            correction_mode: StdMutex::new(TranscriptCorrectionMode::default()),
            corrector: StdMutex::new(None),
            recent_texts: Arc::new(StdMutex::new(VecDeque::new())),
            gate: Arc::new(StdMutex::new(Gate::default())),
            sessions: AsyncMutex::new(HashMap::new()),
            sink,
            counters: Arc::new(RuntimeCounters::default()),
            telemetry,
        }
    }

    pub fn telemetry(&self) -> &Arc<TelemetryRecorder> {
        &self.telemetry
    }

    pub fn settings(&self) -> TranscriptionSettings {
        self.settings.lock().expect("settings mutex").clone()
    }

    /// Trocar configuração **não** reconfigura uma sessão já aberta: a nova valeria a partir
    /// da próxima sessão de transcrição. Reconfigurar no meio produziria uma sessão cujo
    /// começo e fim foram transcritos por backends diferentes.
    pub fn set_settings(&self, settings: TranscriptionSettings) {
        *self.settings.lock().expect("settings mutex") = settings;
    }

    pub fn provider_id(&self) -> crate::transcription::provider::TranscriptionProviderId {
        self.provider.lock().expect("provider mutex").id()
    }

    pub fn set_provider(&self, provider: Arc<dyn TranscriptionProvider>) {
        *self.provider.lock().expect("provider mutex") = provider;
    }

    pub fn set_normalizer(&self, normalizer: Arc<dyn TranscriptNormalizer>) {
        *self.normalizer.lock().expect("normalizer mutex") = normalizer;
    }

    pub fn correction_mode(&self) -> TranscriptCorrectionMode {
        *self.correction_mode.lock().expect("correction mode mutex")
    }

    pub fn set_correction_mode(&self, mode: TranscriptCorrectionMode) {
        if mode == TranscriptCorrectionMode::Contextual && !self.has_contextual_corrector() {
            // Sem corretor contextual registrado nesta build, o modo cai para
            // determinístico na prática. Avisar é melhor que aceitar em silêncio e deixar
            // o usuário achar que uma correção que não existe está rodando.
            warn!(
                "modo de correção contextual selecionado, mas nenhum ContextualCorrector \
                 está registrado; o comportamento efetivo é determinístico"
            );
        }
        *self.correction_mode.lock().expect("correction mode mutex") = mode;
    }

    pub fn has_contextual_corrector(&self) -> bool {
        self.corrector.lock().expect("corrector mutex").is_some()
    }

    /// Ponto de registro da extensão prevista na Parte 6. Nenhuma implementação é registrada
    /// nesta entrega — de propósito, para não colocar uma chamada de LLM entre o fim da fala
    /// e o começo da geração da resposta.
    pub fn set_contextual_corrector(&self, corrector: Option<Arc<dyn ContextualCorrector>>) {
        *self.corrector.lock().expect("corrector mutex") = corrector;
    }

    pub fn stats(&self) -> TranscriptionRuntimeStats {
        TranscriptionRuntimeStats {
            accepted_finals: self.counters.accepted_finals.load(Ordering::Relaxed),
            discarded_no_session: self.counters.discarded_no_session.load(Ordering::Relaxed),
            discarded_stale_session: self
                .counters
                .discarded_stale_session
                .load(Ordering::Relaxed),
            discarded_stale_transcription_session: self
                .counters
                .discarded_stale_transcription_session
                .load(Ordering::Relaxed),
            discarded_duplicate: self.counters.discarded_duplicate.load(Ordering::Relaxed),
            push_errors: self.counters.push_errors.load(Ordering::Relaxed),
        }
    }

    /// Abre a fronteira de uma sessão de conversa. Encerra qualquer sessão de transcrição
    /// remanescente antes — abrir sobre um estado sujo é o mesmo vazamento por outro
    /// caminho.
    pub async fn begin_session(&self, session_id: SessionId) {
        self.end_session().await;
        let mut gate = self.gate.lock().expect("gate mutex");
        gate.session_id = Some(session_id);
        gate.active.clear();
        gate.seen.clear();
    }

    pub async fn end_session(&self) {
        // 1. Bloqueia chunks novos e invalida a identidade: a partir daqui, todo evento que
        //    chegar de um provider já não encontra sessão ativa e é descartado.
        let ending = {
            let mut gate = self.gate.lock().expect("gate mutex");
            let ending = gate.session_id.take();
            gate.active.clear();
            gate.seen.clear();
            ending
        };
        if let Some(ending) = ending {
            // Uma fala interrompida pelo fim da sessão não tem latência de ponta a ponta
            // para reportar; o trace é descartado, não concluído.
            self.telemetry.discard_session(ending);
        }
        // Contexto de correção é conteúdo da conversa: não atravessa a fronteira de sessão.
        self.recent_texts
            .lock()
            .expect("recent texts mutex")
            .clear();

        // 2. Cancela os providers. `cancel`, não `finish`: encerrar sessão significa jogar
        //    fora o que estava em voo, não drenar mais resultados para uma conversa que já
        //    acabou.
        let mut sessions = self.sessions.lock().await;
        for (source, mut active) in sessions.drain() {
            if let Err(e) = active.session.cancel().await {
                debug!(?source, %e, "erro ao cancelar sessão de transcrição");
            }
        }
    }

    /// Encerramento **gracioso** de uma fonte: a captura daquela fonte parou, mas a sessão
    /// de conversa continua (o usuário pode religar o microfone sem encerrar a reunião).
    /// Usa `finish`, não `cancel`: um provider de streaming ainda pode ter uma fala parcial
    /// para fechar, e esses resultados são legítimos — a sessão é a mesma. `end_session`
    /// continua cancelando, porque lá a conversa acabou e o que estava em voo não interessa
    /// mais.
    pub async fn finish_source(&self, source: AudioSource) {
        let active = self.sessions.lock().await.remove(&source);
        let Some(mut active) = active else {
            return;
        };
        if let Err(e) = active.session.finish().await {
            debug!(?source, %e, "erro ao finalizar sessão de transcrição");
        }
        // Só depois de drenar: remover do gate antes faria o próprio resultado que o
        // `finish` acabou de liberar ser descartado como sessão de transcrição obsoleta.
        let mut gate = self.gate.lock().expect("gate mutex");
        if gate.active.get(&source) == Some(&active.id) {
            gate.active.remove(&source);
        }
    }

    pub fn active_session_id(&self) -> Option<SessionId> {
        self.gate.lock().expect("gate mutex").session_id
    }

    /// Ponto de entrada do pipeline de áudio. Devolve `Ok(())` mesmo quando o chunk é
    /// descartado por não haver sessão ativa: isso é comportamento esperado entre sessões,
    /// não uma falha de captura.
    pub async fn push_segment(&self, segment: AudioSegment) -> Result<(), TranscriptionError> {
        self.push_chunk(AudioChunk::from_segment(segment)).await
    }

    pub async fn push_chunk(&self, chunk: AudioChunk) -> Result<(), TranscriptionError> {
        let Some(session_id) = self.active_session_id() else {
            debug!(
                source = ?chunk.source,
                duration_ms = chunk.duration_ms(),
                "áudio recebido sem sessão de conversa ativa; descartado"
            );
            return Ok(());
        };

        let source = chunk.source;
        // O trace da fala nasce aqui, no primeiro chunk, e é o mesmo que o motor de resposta
        // vai fechar lá na frente — é o que permite medir "fim da fala → token visível"
        // atravessando três subsistemas.
        let trace = self.telemetry.begin_or_current(session_id, source);
        self.telemetry.mark(trace, Milestone::FirstAudioChunk);
        self.telemetry.mark(trace, Milestone::LastAudioChunk);
        if let Some(segment_id) = chunk.segment_id {
            self.telemetry.link_segment(trace, segment_id);
        }

        let mut sessions = self.sessions.lock().await;
        let active = match sessions.entry(source) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(self.open_session(session_id, source).await?),
        };
        match active.session.push_audio(chunk).await {
            Ok(()) => Ok(()),
            Err(e) => {
                self.counters.push_errors.fetch_add(1, Ordering::Relaxed);
                // Uma sessão que já não aceita áudio é substituída na próxima entrada em vez
                // de manter uma sessão morta ocupando a fonte.
                if matches!(e, TranscriptionError::SessionClosed) {
                    sessions.remove(&source);
                    self.gate.lock().expect("gate mutex").active.remove(&source);
                }
                Err(e)
            }
        }
    }

    async fn open_session(
        &self,
        session_id: SessionId,
        source: AudioSource,
    ) -> Result<ActiveSession, TranscriptionError> {
        let provider = Arc::clone(&*self.provider.lock().expect("provider mutex"));
        let settings = self.settings();
        let transcription_session_id = TranscriptionSessionId::next();

        // Registrar no gate **antes** de abrir a sessão: um provider pode emitir um evento
        // de dentro de `start_session`, e nesse instante o gate já precisa reconhecer a
        // sessão como ativa.
        {
            let mut gate = self.gate.lock().expect("gate mutex");
            if gate.session_id != Some(session_id) {
                return Err(TranscriptionError::SessionClosed);
            }
            gate.active.insert(source, transcription_session_id);
        }

        let context = TranscriptionSessionContext {
            session_id,
            transcription_session_id,
            source,
            language: settings.language.clone().into(),
            model: settings.model.clone(),
            sink: self.build_sink(),
        };

        match provider.start_session(context).await {
            Ok(session) => Ok(ActiveSession {
                id: transcription_session_id,
                session,
            }),
            Err(e) => {
                let mut gate = self.gate.lock().expect("gate mutex");
                if gate.active.get(&source) == Some(&transcription_session_id) {
                    gate.active.remove(&source);
                }
                Err(e)
            }
        }
    }

    fn build_sink(&self) -> crate::transcription::session::TranscriptionEventSink {
        let gate = Arc::clone(&self.gate);
        let counters = Arc::clone(&self.counters);
        let downstream = Arc::clone(&self.sink);
        let normalizer = Arc::clone(&*self.normalizer.lock().expect("normalizer mutex"));
        let correction_mode = *self.correction_mode.lock().expect("correction mode mutex");
        let telemetry = Arc::clone(&self.telemetry);
        let provider_id = self.provider_id();
        let model = self.settings().model;
        let corrector = self.corrector.lock().expect("corrector mutex").clone();
        let recent_texts = Arc::clone(&self.recent_texts);

        Arc::new(move |event: TranscriptionEvent| {
            let decision = gate.lock().expect("gate mutex").accept(&event);
            if let Err(reason) = decision {
                let counter = match reason {
                    DiscardReason::NoActiveSession => &counters.discarded_no_session,
                    DiscardReason::StaleSession => &counters.discarded_stale_session,
                    DiscardReason::StaleTranscriptionSession => {
                        &counters.discarded_stale_transcription_session
                    }
                    DiscardReason::DuplicateEvent => &counters.discarded_duplicate,
                };
                counter.fetch_add(1, Ordering::Relaxed);
                debug!(
                    ?reason,
                    session_id = event.session_id().value(),
                    transcription_session_id = event.transcription_session_id().0,
                    source = ?event.source(),
                    "evento de transcrição descartado no backend"
                );
                downstream(TranscriptionRuntimeOutput::Discarded {
                    reason,
                    session_id: event.session_id(),
                    transcription_session_id: event.transcription_session_id(),
                    source: event.source(),
                });
                return;
            }

            // O trace já existe (foi aberto em `push_chunk` para esta sessão/fonte);
            // `begin_or_current` aqui é uma busca, não uma criação.
            let trace = telemetry.begin_or_current(event.session_id(), event.source());
            match &event {
                TranscriptionEvent::SpeechStarted(_) => {
                    telemetry.mark(trace, Milestone::SpeechStarted)
                }
                TranscriptionEvent::SpeechEnded(_) => telemetry.mark(trace, Milestone::SpeechEnded),
                TranscriptionEvent::Partial(_) => {
                    telemetry.mark(trace, Milestone::FirstPartialTranscript)
                }
                TranscriptionEvent::Final(_) => telemetry.mark(trace, Milestone::FinalTranscript),
                TranscriptionEvent::Error(_) => {}
            }

            downstream(TranscriptionRuntimeOutput::Event(event.clone()));

            if let TranscriptionEvent::Final(transcript) = event {
                let deterministic = if correction_mode.applies_deterministic() {
                    normalizer.normalize(TranscriptNormalizationInput {
                        raw_text: transcript.text.clone(),
                        source: transcript.source,
                        language: transcript.language.clone(),
                        provider: transcript.provider,
                    })
                } else {
                    TranscriptNormalizationResult::unchanged(transcript.text.clone())
                };

                // Correção contextual, quando (e só quando) houver corretor registrado. Roda
                // numa task própria porque é I/O: mantê-la aqui bloquearia o `push_audio` do
                // provider, e com ele a fonte de áudio inteira.
                let normalization = match (correction_mode.applies_contextual(), &corrector) {
                    (true, Some(corrector)) => {
                        let corrector = Arc::clone(corrector);
                        let downstream = Arc::clone(&downstream);
                        let telemetry = Arc::clone(&telemetry);
                        let recent_texts = Arc::clone(&recent_texts);
                        let counters = Arc::clone(&counters);
                        let transcript = transcript.clone();
                        let recent_context: Vec<String> = recent_texts
                            .lock()
                            .expect("recent texts mutex")
                            .iter()
                            .cloned()
                            .collect();
                        tauri::async_runtime::spawn(async move {
                            let corrected = corrector
                                .correct(ContextualCorrectionInput {
                                    deterministic,
                                    recent_context,
                                })
                                .await;
                            telemetry.mark(trace, Milestone::NormalizationCompleted);
                            remember(&recent_texts, &corrected.normalized_text);
                            counters.accepted_finals.fetch_add(1, Ordering::Relaxed);
                            downstream(TranscriptionRuntimeOutput::Final(Box::new(
                                NormalizedTranscript {
                                    transcript,
                                    normalization: corrected,
                                },
                            )));
                        });
                        return;
                    }
                    _ => deterministic,
                };

                telemetry.mark(trace, Milestone::NormalizationCompleted);
                remember(&recent_texts, &normalization.normalized_text);
                if let Some(segment_id) = transcript.segment_id {
                    // O provider pode ter atribuído um `SegmentId` próprio ao resultado; é
                    // esse que a timeline vai usar para montar a utterance, então é por ele
                    // que o trace precisa ser encontrável.
                    telemetry.link_segment(trace, segment_id);
                }
                telemetry.record_attributes(
                    trace,
                    TraceAttributes {
                        transcription_provider: Some(provider_id.as_str().to_string()),
                        transcription_model: model.clone(),
                        raw_text_length: Some(normalization.raw_text.chars().count()),
                        normalized_text_length: Some(normalization.normalized_text.chars().count()),
                        normalization_change_count: Some(normalization.change_count()),
                        ..Default::default()
                    },
                );
                telemetry.record_text(trace, &normalization.normalized_text);
                counters.accepted_finals.fetch_add(1, Ordering::Relaxed);
                downstream(TranscriptionRuntimeOutput::Final(Box::new(
                    NormalizedTranscript {
                        transcript,
                        normalization,
                    },
                )));
            }
        })
    }

    /// Só para diagnósticos: qual sessão de transcrição está viva em cada fonte.
    pub fn active_transcription_sessions(&self) -> Vec<(AudioSource, TranscriptionSessionId)> {
        let gate = self.gate.lock().expect("gate mutex");
        let mut out: Vec<(AudioSource, TranscriptionSessionId)> =
            gate.active.iter().map(|(s, id)| (*s, *id)).collect();
        out.sort_by_key(|(_, id)| id.0);
        out
    }
}

impl ActiveSession {
    /// Exposto para teste: confirma que a sessão viva é a que o gate reconhece.
    #[cfg(test)]
    fn id(&self) -> TranscriptionSessionId {
        self.id
    }
}

fn remember(recent: &StdMutex<VecDeque<String>>, text: &str) {
    if text.trim().is_empty() {
        return;
    }
    let mut recent = recent.lock().expect("recent texts mutex");
    recent.push_back(text.to_string());
    while recent.len() > CORRECTION_CONTEXT_UTTERANCES {
        recent.pop_front();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::segment::AudioTimestamp;
    use crate::transcription::fake_provider::{FakeBehavior, FakeTranscriptionProvider};
    use crate::transcription::provider::TranscriptionProviderId;
    use std::time::Duration;

    fn collector() -> (
        Arc<StdMutex<Vec<TranscriptionRuntimeOutput>>>,
        TranscriptionOutputSink,
    ) {
        let out = Arc::new(StdMutex::new(Vec::new()));
        let sink_out = Arc::clone(&out);
        let sink: TranscriptionOutputSink =
            Arc::new(move |output| sink_out.lock().unwrap().push(output));
        (out, sink)
    }

    fn chunk(source: AudioSource) -> AudioChunk {
        AudioChunk {
            source,
            samples: vec![0.0; 160],
            sample_rate: 16_000,
            started_at: AudioTimestamp(0),
            ended_at: AudioTimestamp(500),
            segment_id: None,
        }
    }

    fn finals(out: &Arc<StdMutex<Vec<TranscriptionRuntimeOutput>>>) -> Vec<NormalizedTranscript> {
        out.lock()
            .unwrap()
            .iter()
            .filter_map(|o| match o {
                TranscriptionRuntimeOutput::Final(f) => Some((**f).clone()),
                _ => None,
            })
            .collect()
    }

    fn discards(out: &Arc<StdMutex<Vec<TranscriptionRuntimeOutput>>>) -> Vec<DiscardReason> {
        out.lock()
            .unwrap()
            .iter()
            .filter_map(|o| match o {
                TranscriptionRuntimeOutput::Discarded { reason, .. } => Some(*reason),
                _ => None,
            })
            .collect()
    }

    fn runtime_with(
        behavior: FakeBehavior,
        sink: TranscriptionOutputSink,
    ) -> (
        Arc<TranscriptionRuntime>,
        Arc<crate::transcription::fake_provider::FakeProviderLog>,
    ) {
        let provider = Arc::new(FakeTranscriptionProvider::new(behavior));
        let log = provider.log();
        (
            Arc::new(TranscriptionRuntime::new(
                provider,
                TranscriptionSettings::default(),
                sink,
            )),
            log,
        )
    }

    #[tokio::test]
    async fn audio_without_an_active_session_is_dropped_without_opening_a_provider_session() {
        let (out, sink) = collector();
        let (runtime, log) = runtime_with(
            FakeBehavior::EmitsFinal {
                text: "olá".into(),
                partials: false,
            },
            sink,
        );

        runtime
            .push_chunk(chunk(AudioSource::SystemOutput))
            .await
            .unwrap();

        assert!(finals(&out).is_empty());
        assert_eq!(log.sessions().len(), 0);
    }

    #[tokio::test]
    async fn each_source_gets_its_own_transcription_session() {
        let (_out, sink) = collector();
        let (runtime, log) = runtime_with(
            FakeBehavior::EmitsFinal {
                text: "olá".into(),
                partials: false,
            },
            sink,
        );
        runtime.begin_session(SessionId::from_value(7)).await;

        runtime
            .push_chunk(chunk(AudioSource::Microphone))
            .await
            .unwrap();
        runtime
            .push_chunk(chunk(AudioSource::SystemOutput))
            .await
            .unwrap();

        let sessions = log.sessions();
        assert_eq!(sessions.len(), 2);
        let mic = sessions
            .iter()
            .find(|(s, _)| *s == AudioSource::Microphone)
            .unwrap();
        let system = sessions
            .iter()
            .find(|(s, _)| *s == AudioSource::SystemOutput)
            .unwrap();
        assert_ne!(
            mic.1, system.1,
            "microfone e saída de sistema nunca compartilham sessão de transcrição"
        );
    }

    #[tokio::test]
    async fn a_final_transcript_is_normalized_before_leaving_the_runtime() {
        let (out, sink) = collector();
        let (runtime, _log) = runtime_with(
            FakeBehavior::EmitsFinal {
                text: "usamos micro serviços   com ddd".into(),
                partials: false,
            },
            sink,
        );
        runtime.begin_session(SessionId::from_value(1)).await;
        runtime
            .push_chunk(chunk(AudioSource::SystemOutput))
            .await
            .unwrap();

        let finals = finals(&out);
        assert_eq!(finals.len(), 1);
        assert_eq!(
            finals[0].normalization.normalized_text,
            "Usamos microserviços com DDD"
        );
        assert_eq!(
            finals[0].normalization.raw_text, "usamos micro serviços   com ddd",
            "o texto original nunca é descartado"
        );
    }

    #[tokio::test]
    async fn partials_are_forwarded_as_events_but_never_as_finals() {
        let (out, sink) = collector();
        let (runtime, _log) = runtime_with(
            FakeBehavior::EmitsFinal {
                text: "oi".into(),
                partials: true,
            },
            sink,
        );
        runtime.begin_session(SessionId::from_value(1)).await;
        runtime
            .push_chunk(chunk(AudioSource::Microphone))
            .await
            .unwrap();

        assert_eq!(finals(&out).len(), 1);
        let partial_count = out
            .lock()
            .unwrap()
            .iter()
            .filter(|o| {
                matches!(
                    o,
                    TranscriptionRuntimeOutput::Event(TranscriptionEvent::Partial(_))
                )
            })
            .count();
        assert_eq!(partial_count, 1);
    }

    #[tokio::test]
    async fn a_result_arriving_after_the_session_ended_is_discarded_in_the_backend() {
        let (out, sink) = collector();
        let (runtime, _log) = runtime_with(
            FakeBehavior::EmitsFinalAfter {
                text: "tarde demais".into(),
                delay: Duration::from_millis(120),
            },
            sink,
        );
        runtime.begin_session(SessionId::from_value(3)).await;

        let pushing = Arc::clone(&runtime);
        let handle =
            tokio::spawn(async move { pushing.push_chunk(chunk(AudioSource::SystemOutput)).await });

        tokio::time::sleep(Duration::from_millis(20)).await;
        runtime.end_session().await;
        let _ = handle.await;
        tokio::time::sleep(Duration::from_millis(200)).await;

        assert!(
            finals(&out).is_empty(),
            "um resultado de sessão encerrada não pode chegar à timeline"
        );
        assert!(discards(&out).contains(&DiscardReason::NoActiveSession));
    }

    #[tokio::test]
    async fn a_result_from_a_previous_session_is_discarded_after_a_new_one_began() {
        let (out, sink) = collector();
        let (runtime, _log) = runtime_with(
            FakeBehavior::EmitsFinalAfter {
                text: "sessão anterior".into(),
                delay: Duration::from_millis(120),
            },
            sink,
        );
        runtime.begin_session(SessionId::from_value(10)).await;

        let pushing = Arc::clone(&runtime);
        let handle =
            tokio::spawn(async move { pushing.push_chunk(chunk(AudioSource::SystemOutput)).await });

        tokio::time::sleep(Duration::from_millis(20)).await;
        runtime.begin_session(SessionId::from_value(11)).await;
        let _ = handle.await;
        tokio::time::sleep(Duration::from_millis(200)).await;

        assert!(finals(&out).is_empty());
        let reasons = discards(&out);
        assert!(
            reasons.contains(&DiscardReason::NoActiveSession)
                || reasons.contains(&DiscardReason::StaleSession)
                || reasons.contains(&DiscardReason::StaleTranscriptionSession),
            "{reasons:?}"
        );
    }

    #[tokio::test]
    async fn the_same_provider_event_id_is_only_accepted_once() {
        let (out, sink) = collector();
        let (runtime, _log) = runtime_with(
            FakeBehavior::EmitsDuplicate {
                text: "reentrega".into(),
            },
            sink,
        );
        runtime.begin_session(SessionId::from_value(1)).await;
        runtime
            .push_chunk(chunk(AudioSource::SystemOutput))
            .await
            .unwrap();

        assert_eq!(finals(&out).len(), 1, "reentrega não pode duplicar fala");
        assert!(discards(&out).contains(&DiscardReason::DuplicateEvent));
        assert_eq!(runtime.stats().discarded_duplicate, 1);
    }

    #[tokio::test]
    async fn ending_a_session_cancels_every_provider_session() {
        let (_out, sink) = collector();
        let (runtime, log) = runtime_with(FakeBehavior::Silent, sink);
        runtime.begin_session(SessionId::from_value(1)).await;
        runtime
            .push_chunk(chunk(AudioSource::Microphone))
            .await
            .unwrap();
        runtime
            .push_chunk(chunk(AudioSource::SystemOutput))
            .await
            .unwrap();

        runtime.end_session().await;

        assert_eq!(log.cancel_count(), 2);
        assert_eq!(runtime.active_transcription_sessions().len(), 0);
        assert_eq!(runtime.active_session_id(), None);
    }

    #[tokio::test]
    async fn a_new_session_never_reuses_the_previous_transcription_sessions() {
        let (_out, sink) = collector();
        let (runtime, log) = runtime_with(FakeBehavior::Silent, sink);

        runtime.begin_session(SessionId::from_value(1)).await;
        runtime
            .push_chunk(chunk(AudioSource::SystemOutput))
            .await
            .unwrap();
        let first = runtime.active_transcription_sessions();

        runtime.begin_session(SessionId::from_value(2)).await;
        runtime
            .push_chunk(chunk(AudioSource::SystemOutput))
            .await
            .unwrap();
        let second = runtime.active_transcription_sessions();

        assert_ne!(first, second);
        assert_eq!(log.sessions().len(), 2);
        assert_eq!(log.cancel_count(), 1, "a sessão anterior foi cancelada");
    }

    #[tokio::test]
    async fn a_failing_provider_surfaces_the_error_instead_of_falling_back() {
        let (_out, sink) = collector();
        let provider =
            Arc::new(FakeTranscriptionProvider::new(FakeBehavior::Silent).failing_to_start());
        let runtime = TranscriptionRuntime::new(provider, TranscriptionSettings::default(), sink);
        runtime.begin_session(SessionId::from_value(1)).await;

        let err = runtime
            .push_chunk(chunk(AudioSource::SystemOutput))
            .await
            .unwrap_err();
        assert!(matches!(err, TranscriptionError::ProviderUnavailable(_)));
        assert_eq!(runtime.active_transcription_sessions().len(), 0);
    }

    #[tokio::test]
    async fn disabling_normalization_passes_the_raw_text_through() {
        let (out, sink) = collector();
        let (runtime, _log) = runtime_with(
            FakeBehavior::EmitsFinal {
                text: "usamos micro serviços".into(),
                partials: false,
            },
            sink,
        );
        runtime.set_correction_mode(TranscriptCorrectionMode::Disabled);
        runtime.begin_session(SessionId::from_value(1)).await;
        runtime
            .push_chunk(chunk(AudioSource::SystemOutput))
            .await
            .unwrap();

        let finals = finals(&out);
        assert_eq!(
            finals[0].normalization.normalized_text,
            "usamos micro serviços"
        );
        assert_eq!(finals[0].normalization.change_count(), 0);
    }

    #[tokio::test]
    async fn provider_id_reflects_the_configured_backend() {
        let (_out, sink) = collector();
        let (runtime, _log) = runtime_with(FakeBehavior::Silent, sink);
        assert_eq!(runtime.provider_id(), TranscriptionProviderId::Fake);
    }

    #[tokio::test]
    async fn active_session_ids_are_reported_for_diagnostics() {
        let (_out, sink) = collector();
        let (runtime, _log) = runtime_with(FakeBehavior::Silent, sink);
        runtime.begin_session(SessionId::from_value(1)).await;
        runtime
            .push_chunk(chunk(AudioSource::Microphone))
            .await
            .unwrap();

        let active = runtime.active_transcription_sessions();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].0, AudioSource::Microphone);
        let sessions = runtime.sessions.lock().await;
        assert_eq!(sessions[&AudioSource::Microphone].id(), active[0].1);
    }

    /// As duas fontes compartilham provider, runtime e fila. O microfone é o caminho que
    /// mais falha na prática (dispositivo trocado, permissão revogada) — e é o menos
    /// importante dos dois: quem faz a pergunta é a outra pessoa, pela saída do sistema.
    /// Uma falha no microfone que levasse a saída do sistema junto produziria uma reunião
    /// inteira sem sugestão nenhuma, sem nada na tela explicando o porquê.
    #[tokio::test]
    async fn a_microphone_failure_never_silently_takes_system_output_down_with_it() {
        let (out, sink) = collector();
        let provider = Arc::new(
            FakeTranscriptionProvider::new(FakeBehavior::EmitsFinal {
                text: "a outra pessoa perguntou".into(),
                partials: false,
            })
            .failing_only_for(AudioSource::Microphone),
        );
        let log = provider.log();
        let runtime = TranscriptionRuntime::new(provider, TranscriptionSettings::default(), sink);
        runtime.begin_session(SessionId::from_value(1)).await;

        let mic = runtime.push_chunk(chunk(AudioSource::Microphone)).await;
        assert!(matches!(mic, Err(TranscriptionError::InferenceFailed(_))));

        runtime
            .push_chunk(chunk(AudioSource::SystemOutput))
            .await
            .expect("saída do sistema não pode ser afetada pela falha do microfone");

        let finals = finals(&out);
        assert_eq!(finals.len(), 1, "a fala da outra pessoa chegou");
        assert_eq!(finals[0].transcript.source, AudioSource::SystemOutput);
        // A falha do microfone é reportada, não engolida — o `Error` chega ao sink com a
        // fonte correta e o `lib.rs` o transforma em log e evento para o frontend.
        let errors: Vec<_> = out
            .lock()
            .unwrap()
            .iter()
            .filter_map(|o| match o {
                TranscriptionRuntimeOutput::Event(TranscriptionEvent::Error(e)) => Some(e.source),
                _ => None,
            })
            .collect();
        assert_eq!(errors, vec![AudioSource::Microphone]);
        assert_eq!(
            runtime.active_transcription_sessions().len(),
            2,
            "nenhuma das duas sessões foi derrubada"
        );
        assert_eq!(log.cancel_count(), 0);
    }

    /// Encerrar duas vezes acontece de verdade: o usuário clica em "encerrar" e o mesmo
    /// caminho é acionado pela parada da captura. A segunda passagem não pode cancelar
    /// sessões já canceladas de novo, nem entrar em pânico por não achá-las.
    #[tokio::test]
    async fn cancelling_twice_is_idempotent() {
        let (_out, sink) = collector();
        let (runtime, log) = runtime_with(FakeBehavior::Silent, sink);
        runtime.begin_session(SessionId::from_value(1)).await;
        runtime
            .push_chunk(chunk(AudioSource::Microphone))
            .await
            .unwrap();
        runtime
            .push_chunk(chunk(AudioSource::SystemOutput))
            .await
            .unwrap();

        runtime.end_session().await;
        runtime.end_session().await;

        assert_eq!(log.cancel_count(), 2, "uma vez por fonte, não duas");
        assert_eq!(runtime.active_transcription_sessions().len(), 0);
        assert_eq!(runtime.active_session_id(), None);

        // E o áudio que chega depois continua sendo descartado, não reabre nada.
        let late = runtime.push_chunk(chunk(AudioSource::SystemOutput)).await;
        assert!(late.is_ok(), "chunk tardio é descartado, não é erro");
        assert_eq!(runtime.active_transcription_sessions().len(), 0);
    }
}
