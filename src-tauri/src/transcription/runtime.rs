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

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Instant;

use serde::Serialize;
use tokio::sync::Mutex as AsyncMutex;
use tracing::{debug, warn};

use crate::audio::segment::{AudioSegment, SegmentId};
use crate::audio::types::{AudioSource, CaptureStreamId};
use crate::conversation::SessionId;
use crate::integrity::{
    text_hash, IntegrityStage, IntegrityStatus, OriginIntegrityLog, OriginObservation,
    SourceIntegrityError,
};
use crate::normalization::{
    ContextualCorrectionInput, ContextualCorrector, DeterministicNormalizer,
    TranscriptCorrectionMode, TranscriptNormalizationInput, TranscriptNormalizationResult,
    TranscriptNormalizer,
};
use crate::telemetry::{Milestone, TelemetryRecorder, TraceAttributes};
use crate::transcription::envelope::{
    MonotonicTimestamp, PendingSegmentIdentity, TranscriptionResultEnvelope,
    TranscriptionStreamKey, TranscriptionWorkItem,
};
use crate::transcription::error::TranscriptionError;
use crate::transcription::events::{FinalTranscript, ProviderEventId, TranscriptionEvent};
use crate::transcription::provider::{TranscriptionCapabilities, TranscriptionProvider};
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
    /// Identidade causal do segmento que originou este texto, copiada da fila de pendentes
    /// do próprio fluxo de captura. **É esta a autoridade sobre a origem**, não
    /// `transcript.source`: o provider devolve texto para um envelope, não decide de onde o
    /// áudio veio. Ver `transcription::envelope`.
    pub envelope: TranscriptionResultEnvelope,
    pub transcript: FinalTranscript,
    pub normalization: TranscriptNormalizationResult,
    /// Instante monotônico em que o segmento deixou a captura e entrou na fila.
    /// Preservá-lo permite que freshness inclua backlog e inferência.
    pub speech_ended_at: Instant,
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
    discarded_stale_configuration: AtomicU64,
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
    pub discarded_stale_configuration: u64,
    pub push_errors: u64,
}

/// Estado consultado de forma **síncrona** por cada evento que chega de um provider. Fica
/// num `std::sync::Mutex` separado do mapa de sessões (que é `async`) de propósito: o filtro
/// precisa decidir sem `await`, senão um evento obsoleto poderia atravessar a fronteira
/// enquanto espera o lock assíncrono.
#[derive(Default)]
struct Gate {
    session_id: Option<SessionId>,
    active: HashMap<AudioSource, ActiveTranscriptionIdentity>,
    seen: HashMap<TranscriptionSessionId, (HashSet<ProviderEventId>, VecDeque<ProviderEventId>)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ActiveTranscriptionIdentity {
    session_id: TranscriptionSessionId,
    capture_stream_id: CaptureStreamId,
}

impl Gate {
    fn accept(
        &mut self,
        event: &TranscriptionEvent,
    ) -> Result<ActiveTranscriptionIdentity, DiscardReason> {
        let Some(current) = self.session_id else {
            return Err(DiscardReason::NoActiveSession);
        };
        if event.session_id() != current {
            return Err(DiscardReason::StaleSession);
        }
        let transcription_session_id = event.transcription_session_id();
        let active = match self.active.get(&event.source()).copied() {
            Some(active) if active.session_id == transcription_session_id => active,
            _ => return Err(DiscardReason::StaleTranscriptionSession),
        };
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
        Ok(active)
    }
}

struct ActiveSession {
    id: TranscriptionSessionId,
    session: Box<dyn TranscriptionSession>,
}

struct SourceSessions {
    microphone: AsyncMutex<Option<ActiveSession>>,
    system_output: AsyncMutex<Option<ActiveSession>>,
}

impl SourceSessions {
    fn new() -> Self {
        SourceSessions {
            microphone: AsyncMutex::new(None),
            system_output: AsyncMutex::new(None),
        }
    }

    fn get(&self, source: AudioSource) -> &AsyncMutex<Option<ActiveSession>> {
        match source {
            AudioSource::Microphone => &self.microphone,
            AudioSource::SystemOutput => &self.system_output,
        }
    }
}

/// Uma fila de identidades pendentes **por fluxo de captura**, nunca uma fila global.
///
/// A chave carrega `session_id + source + capture_stream_id` justamente para que o fallback
/// por ordem de chegada — necessário para providers que não devolvem `segment_id` — nunca
/// possa casar um resultado do microfone com um segmento da saída de sistema. Com uma fila
/// única, bastaria a inferência do microfone terminar primeiro para a fala da outra pessoa
/// herdar a identidade errada, e a partir daí toda a cadeia (speaker, elegibilidade,
/// geração) estaria coerentemente errada.
type PendingSegmentTimings = HashMap<TranscriptionStreamKey, VecDeque<PendingSegmentIdentity>>;

/// Como a identidade de um resultado foi encontrada. O fallback é registrado explicitamente
/// porque atribuição por ordem é mais fraca que atribuição por id, e essa diferença precisa
/// aparecer em diagnóstico em vez de ficar implícita.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdentityResolution {
    BySegmentId,
    ByStreamFifo,
}

/// Localiza a identidade de um resultado dentro do **seu próprio** fluxo.
///
/// Só filas cuja chave bate em `session_id` e `source` são consideradas; `capture_stream_id`
/// não vem no evento do provider, então quando há mais de um fluxo vivo para a mesma fonte
/// (janela curta de troca de dispositivo) o desempate é pelo segmento capturado há mais
/// tempo — o mais antigo é o que está esperando resultado há mais tempo.
fn resolve_pending_identity(
    pending: &mut PendingSegmentTimings,
    session_id: SessionId,
    source: AudioSource,
    segment_id: Option<SegmentId>,
) -> Option<(PendingSegmentIdentity, IdentityResolution)> {
    let keys: Vec<TranscriptionStreamKey> = pending
        .keys()
        .filter(|key| key.session_id == session_id && key.source == source)
        .copied()
        .collect();

    if let Some(segment_id) = segment_id {
        for key in &keys {
            let Some(queue) = pending.get_mut(key) else {
                continue;
            };
            if let Some(position) = queue
                .iter()
                .position(|identity| identity.segment_id == segment_id)
            {
                if let Some(identity) = queue.remove(position) {
                    return Some((identity, IdentityResolution::BySegmentId));
                }
            }
        }
    }

    let oldest = keys
        .iter()
        .filter_map(|key| {
            pending
                .get(key)
                .and_then(|queue| queue.front())
                .map(|identity| (*key, identity.captured_at))
        })
        .min_by_key(|(_, captured_at)| *captured_at)
        .map(|(key, _)| key)?;

    pending
        .get_mut(&oldest)
        .and_then(VecDeque::pop_front)
        .map(|identity| (identity, IdentityResolution::ByStreamFifo))
}

struct RuntimeConfiguration {
    provider: Arc<dyn TranscriptionProvider>,
    settings: TranscriptionSettings,
}

pub struct TranscriptionRuntime {
    /// Provider e settings precisam mudar como uma unidade. Duas travas permitiam abrir
    /// uma sessão com provider novo e modelo/idioma antigos durante troca dinâmica.
    configuration: StdMutex<RuntimeConfiguration>,
    /// Monotonic routing epoch captured by queue items. A provider swap must
    /// never feed already-buffered audio into the replacement provider.
    configuration_revision: AtomicU64,
    normalizer: StdMutex<Arc<dyn TranscriptNormalizer>>,
    correction_mode: StdMutex<TranscriptCorrectionMode>,
    /// `None` em toda build atual — ver `normalization::correction`. O campo existe para que
    /// a extensão seja um registro, não uma refatoração.
    corrector: StdMutex<Option<Arc<dyn ContextualCorrector>>>,
    recent_texts: Arc<StdMutex<VecDeque<String>>>,
    pending_segment_timings: Arc<StdMutex<PendingSegmentTimings>>,
    gate: Arc<StdMutex<Gate>>,
    /// Uma trava por fonte. Providers de rede podem bloquear em I/O; isso jamais deve
    /// impedir a outra fonte de alimentar sua própria sessão.
    sessions: SourceSessions,
    sink: TranscriptionOutputSink,
    counters: Arc<RuntimeCounters>,
    telemetry: Arc<TelemetryRecorder>,
    origin_log: Arc<OriginIntegrityLog>,
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
        Self::with_telemetry_and_origin_log(
            provider,
            settings,
            sink,
            telemetry,
            Arc::clone(crate::integrity::origin_log()),
        )
    }

    /// Mesma razão de `with_telemetry`: um teste que queira observar o rastro de origem
    /// precisa de um log próprio, não do singleton de processo que outros testes rodando em
    /// paralelo também tocariam.
    pub fn with_telemetry_and_origin_log(
        provider: Arc<dyn TranscriptionProvider>,
        settings: TranscriptionSettings,
        sink: TranscriptionOutputSink,
        telemetry: Arc<TelemetryRecorder>,
        origin_log: Arc<OriginIntegrityLog>,
    ) -> Self {
        TranscriptionRuntime {
            configuration: StdMutex::new(RuntimeConfiguration { provider, settings }),
            // Even revisions are stable; odd revisions mean a provider swap
            // is in progress and all ingress must be rejected.
            configuration_revision: AtomicU64::new(0),
            normalizer: StdMutex::new(Arc::new(DeterministicNormalizer::default())),
            correction_mode: StdMutex::new(TranscriptCorrectionMode::default()),
            corrector: StdMutex::new(None),
            recent_texts: Arc::new(StdMutex::new(VecDeque::new())),
            pending_segment_timings: Arc::new(StdMutex::new(HashMap::new())),
            gate: Arc::new(StdMutex::new(Gate::default())),
            sessions: SourceSessions::new(),
            sink,
            counters: Arc::new(RuntimeCounters::default()),
            telemetry,
            origin_log,
        }
    }

    pub fn telemetry(&self) -> &Arc<TelemetryRecorder> {
        &self.telemetry
    }

    pub fn origin_log(&self) -> &Arc<OriginIntegrityLog> {
        &self.origin_log
    }

    pub fn settings(&self) -> TranscriptionSettings {
        self.configuration
            .lock()
            .expect("transcription configuration mutex")
            .settings
            .clone()
    }

    pub fn configuration_revision(&self) -> u64 {
        self.configuration_revision.load(Ordering::Acquire)
    }

    fn is_stale_configuration(&self, expected_revision: Option<u64>) -> bool {
        let current_revision = self.configuration_revision();
        let stale = current_revision % 2 == 1
            || expected_revision.is_some_and(|expected| expected != current_revision);
        if stale {
            self.counters
                .discarded_stale_configuration
                .fetch_add(1, Ordering::Relaxed);
        }
        stale
    }

    /// Trocar configuração **não** reconfigura uma sessão já aberta: a nova valeria a partir
    /// da próxima sessão de transcrição. Reconfigurar no meio produziria uma sessão cujo
    /// começo e fim foram transcritos por backends diferentes.
    pub fn set_settings(&self, settings: TranscriptionSettings) {
        self.configuration
            .lock()
            .expect("transcription configuration mutex")
            .settings = settings;
    }

    pub fn provider_id(&self) -> crate::transcription::provider::TranscriptionProviderId {
        self.configuration
            .lock()
            .expect("transcription configuration mutex")
            .provider
            .id()
    }

    pub fn provider_capabilities(&self) -> TranscriptionCapabilities {
        self.configuration
            .lock()
            .expect("transcription configuration mutex")
            .provider
            .capabilities()
    }

    pub fn set_provider(&self, provider: Arc<dyn TranscriptionProvider>) {
        self.configuration
            .lock()
            .expect("transcription configuration mutex")
            .provider = provider;
    }

    pub fn set_provider_and_settings(
        &self,
        provider: Arc<dyn TranscriptionProvider>,
        settings: TranscriptionSettings,
    ) {
        *self
            .configuration
            .lock()
            .expect("transcription configuration mutex") =
            RuntimeConfiguration { provider, settings };
    }

    /// Troca dinâmica: invalida primeiro as identidades antigas, instala provider e
    /// settings atomicamente e só então cancela as sessões por fonte. O próximo item de
    /// cada lane abre uma sessão nova; nenhum evento do provider anterior atravessa a
    /// fronteira.
    pub async fn reconfigure(
        &self,
        provider: Arc<dyn TranscriptionProvider>,
        settings: TranscriptionSettings,
    ) {
        {
            let mut gate = self.gate.lock().expect("gate mutex");
            // The revision transition and event-gate invalidation share this
            // critical section with `open_session`, which is the linearization
            // point for the swap.
            self.configuration_revision.fetch_add(1, Ordering::AcqRel);
            gate.active.clear();
            gate.seen.clear();
        }
        self.pending_segment_timings
            .lock()
            .expect("segment timing mutex")
            .clear();
        tokio::join!(
            self.cancel_source_session(AudioSource::Microphone),
            self.cancel_source_session(AudioSource::SystemOutput),
        );
        self.set_provider_and_settings(provider, settings);
        self.configuration_revision.fetch_add(1, Ordering::Release);
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
            discarded_stale_configuration: self
                .counters
                .discarded_stale_configuration
                .load(Ordering::Relaxed),
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
        self.pending_segment_timings
            .lock()
            .expect("segment timing mutex")
            .clear();

        // 2. Cancela os providers. `cancel`, não `finish`: encerrar sessão significa jogar
        //    fora o que estava em voo, não drenar mais resultados para uma conversa que já
        //    acabou.
        tokio::join!(
            self.cancel_source_session(AudioSource::Microphone),
            self.cancel_source_session(AudioSource::SystemOutput),
        );
    }

    async fn cancel_source_session(&self, source: AudioSource) {
        let mut slot = self.sessions.get(source).lock().await;
        let Some(mut active) = slot.take() else {
            return;
        };
        if let Err(error) = active.session.cancel().await {
            debug!(?source, %error, "erro ao cancelar sessão de transcrição");
        }
    }

    /// Encerramento **gracioso** de uma fonte: a captura daquela fonte parou, mas a sessão
    /// de conversa continua (o usuário pode religar o microfone sem encerrar a reunião).
    /// Usa `finish`, não `cancel`: um provider de streaming ainda pode ter uma fala parcial
    /// para fechar, e esses resultados são legítimos — a sessão é a mesma. `end_session`
    /// continua cancelando, porque lá a conversa acabou e o que estava em voo não interessa
    /// mais.
    pub async fn finish_source(&self, source: AudioSource) {
        let mut slot = self.sessions.get(source).lock().await;
        let Some(mut active) = slot.take() else {
            return;
        };
        if let Err(e) = active.session.finish().await {
            debug!(?source, %e, "erro ao finalizar sessão de transcrição");
        }
        // Só depois de drenar: remover do gate antes faria o próprio resultado que o
        // `finish` acabou de liberar ser descartado como sessão de transcrição obsoleta.
        let mut gate = self.gate.lock().expect("gate mutex");
        if gate
            .active
            .get(&source)
            .is_some_and(|identity| identity.session_id == active.id)
        {
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
        self.push_segment_at(segment, Instant::now()).await
    }

    /// Igual a `push_segment`, mas conserva o instante real de entrada na fila. Envolve o
    /// segmento no envelope usando a sessão ativa; sem sessão ativa o áudio é descartado
    /// como já era (não é falha de captura, é o intervalo entre sessões).
    pub async fn push_segment_at(
        &self,
        segment: AudioSegment,
        speech_ended_at: Instant,
    ) -> Result<(), TranscriptionError> {
        let Some(session_id) = self.active_session_id() else {
            debug!(
                source = ?segment.source,
                "áudio recebido sem sessão de conversa ativa; descartado"
            );
            return Ok(());
        };
        let at = MonotonicTimestamp::from_instant(speech_ended_at);
        self.push_work_item(TranscriptionWorkItem::from_segment(
            session_id, segment, at, at,
        ))
        .await
    }

    /// Ponto de entrada do envelope causal. A identidade é registrada na fila do **seu**
    /// fluxo antes de o áudio ir para o provider, para que o resultado — que volta por outro
    /// caminho, de forma assíncrona — tenha onde se ancorar.
    pub async fn push_work_item(
        &self,
        item: TranscriptionWorkItem,
    ) -> Result<(), TranscriptionError> {
        let revision = self.configuration_revision();
        self.push_work_item_inner(item, Some(revision)).await
    }

    pub async fn push_work_item_for_revision(
        &self,
        item: TranscriptionWorkItem,
        expected_revision: u64,
    ) -> Result<(), TranscriptionError> {
        self.push_work_item_inner(item, Some(expected_revision))
            .await
    }

    async fn push_work_item_inner(
        &self,
        item: TranscriptionWorkItem,
        expected_revision: Option<u64>,
    ) -> Result<(), TranscriptionError> {
        if self.is_stale_configuration(expected_revision) {
            return Err(TranscriptionError::Cancelled);
        }
        let identity = item.identity();
        let key = item.stream_key();
        let trace = self
            .telemetry
            .begin_or_current(item.session_id, item.source);
        self.telemetry.link_segment(trace, item.segment_id);
        self.telemetry.record_attributes(
            trace,
            TraceAttributes {
                transcription_queue_wait_ms: Some(item.enqueued_at.elapsed_ms()),
                ..Default::default()
            },
        );
        self.pending_segment_timings
            .lock()
            .expect("segment timing mutex")
            .entry(key)
            .or_default()
            .push_back(identity);

        let result = self
            .push_chunk_inner(AudioChunk::from_segment(item.audio), expected_revision)
            .await;
        if result.is_err() {
            if let Some(pending) = self
                .pending_segment_timings
                .lock()
                .expect("segment timing mutex")
                .get_mut(&key)
            {
                pending.retain(|pending| pending.segment_id != identity.segment_id);
            }
        }
        result
    }

    pub async fn push_chunk(&self, chunk: AudioChunk) -> Result<(), TranscriptionError> {
        let revision = self.configuration_revision();
        self.push_chunk_inner(chunk, Some(revision)).await
    }

    pub async fn push_chunk_for_revision(
        &self,
        chunk: AudioChunk,
        expected_revision: u64,
    ) -> Result<(), TranscriptionError> {
        self.push_chunk_inner(chunk, Some(expected_revision)).await
    }

    async fn push_chunk_inner(
        &self,
        chunk: AudioChunk,
        expected_revision: Option<u64>,
    ) -> Result<(), TranscriptionError> {
        let Some(session_id) = self.active_session_id() else {
            debug!(
                source = ?chunk.source,
                duration_ms = chunk.duration_ms(),
                "áudio recebido sem sessão de conversa ativa; descartado"
            );
            return Ok(());
        };

        let source = chunk.source;
        let capture_stream_id = chunk.capture_stream_id;
        // O trace da fala nasce aqui, no primeiro chunk, e é o mesmo que o motor de resposta
        // vai fechar lá na frente — é o que permite medir "fim da fala → token visível"
        // atravessando três subsistemas.
        let trace = self.telemetry.begin_or_current(session_id, source);
        self.telemetry.mark(trace, Milestone::FirstAudioChunk);
        self.telemetry.mark(trace, Milestone::LastAudioChunk);
        if let Some(segment_id) = chunk.segment_id {
            self.telemetry.link_segment(trace, segment_id);
        }

        let mut slot = self.sessions.get(source).lock().await;
        if self.is_stale_configuration(expected_revision) {
            return Err(TranscriptionError::Cancelled);
        }
        if slot.is_none() {
            *slot = Some(
                self.open_session(session_id, source, capture_stream_id, expected_revision)
                    .await?,
            );
        }
        let active = slot.as_mut().expect("session inserted above");
        match active.session.push_audio(chunk).await {
            Ok(()) => Ok(()),
            Err(e) => {
                self.counters.push_errors.fetch_add(1, Ordering::Relaxed);
                // Uma sessão que já não aceita áudio é substituída na próxima entrada em vez
                // de manter uma sessão morta ocupando a fonte.
                if matches!(e, TranscriptionError::SessionClosed) {
                    *slot = None;
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
        capture_stream_id: CaptureStreamId,
        expected_revision: Option<u64>,
    ) -> Result<ActiveSession, TranscriptionError> {
        let (provider, settings) = {
            let configuration = self
                .configuration
                .lock()
                .expect("transcription configuration mutex");
            (
                Arc::clone(&configuration.provider),
                configuration.settings.clone(),
            )
        };
        let transcription_session_id = TranscriptionSessionId::next();

        // Registrar no gate **antes** de abrir a sessão: um provider pode emitir um evento
        // de dentro de `start_session`, e nesse instante o gate já precisa reconhecer a
        // sessão como ativa.
        {
            let mut gate = self.gate.lock().expect("gate mutex");
            if self.is_stale_configuration(expected_revision) {
                return Err(TranscriptionError::Cancelled);
            }
            if gate.session_id != Some(session_id) {
                return Err(TranscriptionError::SessionClosed);
            }
            gate.active.insert(
                source,
                ActiveTranscriptionIdentity {
                    session_id: transcription_session_id,
                    capture_stream_id,
                },
            );
        }

        let context = TranscriptionSessionContext {
            session_id,
            transcription_session_id,
            source,
            language: settings.language.clone().into(),
            model: settings.active_model(),
            sink: self.build_sink(),
        };

        match provider.start_session(context).await {
            Ok(session) => Ok(ActiveSession {
                id: transcription_session_id,
                session,
            }),
            Err(e) => {
                let mut gate = self.gate.lock().expect("gate mutex");
                if gate
                    .active
                    .get(&source)
                    .is_some_and(|identity| identity.session_id == transcription_session_id)
                {
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
        let (provider_id, model) = {
            let configuration = self
                .configuration
                .lock()
                .expect("transcription configuration mutex");
            (
                configuration.provider.id(),
                configuration.settings.active_model(),
            )
        };
        let corrector = self.corrector.lock().expect("corrector mutex").clone();
        let recent_texts = Arc::clone(&self.recent_texts);
        let pending_segment_timings = Arc::clone(&self.pending_segment_timings);
        let origin_log = Arc::clone(&self.origin_log);

        Arc::new(move |event: TranscriptionEvent| {
            let active_identity = match gate.lock().expect("gate mutex").accept(&event) {
                Ok(identity) => identity,
                Err(reason) => {
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
            };

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
                // A identidade é procurada **na fila do fluxo que produziu este áudio**, e a
                // busca é filtrada por sessão e fonte antes de qualquer coisa: um resultado
                // do microfone não tem como consumir a identidade de um segmento da saída de
                // sistema nem quando o provider não devolve `segment_id`.
                let resolved = {
                    let mut timings = pending_segment_timings
                        .lock()
                        .expect("segment timing mutex");
                    resolve_pending_identity(
                        &mut timings,
                        transcript.session_id,
                        transcript.source,
                        transcript.segment_id,
                    )
                };

                let (identity, resolution) = match resolved {
                    Some(resolved) => resolved,
                    None => {
                        // Sem identidade registrada não há de onde tirar a origem com
                        // autoridade. Reconstruí-la a partir do que o provider afirmou seria
                        // exatamente a inferência que este pipeline deixou de fazer — mas
                        // descartar a fala também não é aceitável, então a identidade é
                        // sintetizada a partir do próprio evento e marcada como fallback.
                        let synthetic = PendingSegmentIdentity {
                            session_id: transcript.session_id,
                            segment_id: transcript.segment_id.unwrap_or_else(SegmentId::next),
                            source: transcript.source,
                            capture_stream_id: active_identity.capture_stream_id,
                            sequence_number: 0,
                            captured_at: MonotonicTimestamp::now(),
                            enqueued_at: MonotonicTimestamp::now(),
                        };
                        debug!(
                            session_id = transcript.session_id.value(),
                            source = ?transcript.source,
                            "resultado final sem identidade pendente registrada; \
                             origem mantida como a do evento"
                        );
                        (synthetic, IdentityResolution::ByStreamFifo)
                    }
                };

                // Comparação explícita: o provider *reportou* uma fonte, a captura
                // *registrou* outra. Isso nunca é reconciliado — o resultado é rejeitado e
                // a violação vira erro estruturado. Corrigir aqui significaria deixar entrar
                // um dado cuja origem real já é desconhecida.
                if let Err(error) = SourceIntegrityError::check(
                    identity.segment_id,
                    identity.source,
                    transcript.source,
                    IntegrityStage::TranscriptionResult,
                ) {
                    origin_log.record_violation(error);
                    return;
                }

                let speech_ended_at = identity.captured_at.as_instant();
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
                        let origin_log = Arc::clone(&origin_log);
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
                            let envelope = TranscriptionResultEnvelope::from_identity(identity);
                            record_origin_observation(
                                &origin_log,
                                &identity,
                                &transcript,
                                &corrected,
                                resolution,
                            );
                            downstream(TranscriptionRuntimeOutput::Final(Box::new(
                                NormalizedTranscript {
                                    envelope,
                                    transcript,
                                    normalization: corrected,
                                    speech_ended_at,
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
                let envelope = TranscriptionResultEnvelope::from_identity(identity);
                record_origin_observation(
                    &origin_log,
                    &identity,
                    &transcript,
                    &normalization,
                    resolution,
                );
                downstream(TranscriptionRuntimeOutput::Final(Box::new(
                    NormalizedTranscript {
                        envelope,
                        transcript,
                        normalization,
                        speech_ended_at,
                    },
                )));
            }
        })
    }

    /// Só para diagnósticos: qual sessão de transcrição está viva em cada fonte.
    pub fn active_transcription_sessions(&self) -> Vec<(AudioSource, TranscriptionSessionId)> {
        let gate = self.gate.lock().expect("gate mutex");
        let mut out: Vec<(AudioSource, TranscriptionSessionId)> = gate
            .active
            .iter()
            .map(|(source, identity)| (*source, identity.session_id))
            .collect();
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

/// Grava o rastro de origem deste resultado. `source_at_timeline`/`derived_speaker` ficam
/// `None` aqui de propósito: quem os conhece é a timeline, e preenchê-los com uma suposição
/// tiraria da observação justamente o poder de mostrar uma divergência entre estágios.
///
/// Nenhum texto entra no registro — só hashes (ver `integrity::text_hash`).
fn record_origin_observation(
    origin_log: &OriginIntegrityLog,
    identity: &PendingSegmentIdentity,
    transcript: &FinalTranscript,
    normalization: &TranscriptNormalizationResult,
    resolution: IdentityResolution,
) {
    origin_log.record(OriginObservation {
        session_id: identity.session_id.value(),
        capture_stream_id: identity.capture_stream_id.value(),
        segment_id: identity.segment_id,
        sequence_number: identity.sequence_number,
        source_at_capture: identity.source,
        source_at_queue: identity.source,
        source_at_transcription_result: transcript.source,
        source_at_timeline: None,
        derived_speaker: None,
        audio_started_at_ms: transcript.started_at.0,
        audio_ended_at_ms: transcript.ended_at.0,
        transcription_completed_at_ms: identity.enqueued_at.elapsed_ms(),
        raw_text_hash: text_hash(&normalization.raw_text),
        normalized_text_hash: text_hash(&normalization.normalized_text),
        cross_source_similarity: None,
        integrity_status: match resolution {
            IdentityResolution::BySegmentId => IntegrityStatus::Ok,
            IdentityResolution::ByStreamFifo => IntegrityStatus::ResolvedByFifoFallback,
        },
    });
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
    use crate::audio::types::CaptureStreamId;
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
            capture_stream_id: CaptureStreamId::UNASSIGNED,
            sequence_number: 0,
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
    async fn queued_segment_preserves_original_speech_end_for_freshness() {
        let (out, sink) = collector();
        let (runtime, _log) = runtime_with(
            FakeBehavior::EmitsFinal {
                text: "pergunta atual".into(),
                partials: false,
            },
            sink,
        );
        runtime.begin_session(SessionId::from_value(1)).await;
        let speech_ended_at = Instant::now() - Duration::from_secs(12);
        let segment = AudioSegment::new(
            AudioSource::SystemOutput,
            vec![0.0; 160],
            16_000,
            AudioTimestamp(0),
            AudioTimestamp(500),
        );

        runtime
            .push_segment_at(segment, speech_ended_at)
            .await
            .unwrap();

        let finals = finals(&out);
        assert_eq!(finals.len(), 1);
        assert_eq!(finals[0].speech_ended_at, speech_ended_at);
        assert!(finals[0].speech_ended_at.elapsed() >= Duration::from_secs(12));
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
        let session = runtime.sessions.get(AudioSource::Microphone).lock().await;
        assert_eq!(session.as_ref().unwrap().id(), active[0].1);
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

    #[tokio::test(start_paused = true)]
    async fn slow_provider_io_on_one_source_does_not_block_the_other_source() {
        let (out, sink) = collector();
        let (runtime, _log) = runtime_with(
            FakeBehavior::EmitsFinalAfter {
                text: "resultado".into(),
                delay: Duration::from_millis(120),
            },
            sink,
        );
        runtime.begin_session(SessionId::from_value(1)).await;

        let microphone_runtime = Arc::clone(&runtime);
        let microphone = tokio::spawn(async move {
            microphone_runtime
                .push_chunk(chunk(AudioSource::Microphone))
                .await
        });
        let system_runtime = Arc::clone(&runtime);
        let system_output = tokio::spawn(async move {
            system_runtime
                .push_chunk(chunk(AudioSource::SystemOutput))
                .await
        });

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(120)).await;
        tokio::task::yield_now().await;

        assert!(microphone.is_finished());
        assert!(system_output.is_finished());
        microphone.await.unwrap().unwrap();
        system_output.await.unwrap().unwrap();
        assert_eq!(finals(&out).len(), 2);
    }

    #[tokio::test]
    async fn dynamic_reconfigure_cancels_old_sessions_before_new_audio_is_accepted() {
        let (out, sink) = collector();
        let first = Arc::new(FakeTranscriptionProvider::new(FakeBehavior::Silent));
        let first_log = first.log();
        let runtime = Arc::new(TranscriptionRuntime::new(
            first,
            TranscriptionSettings::default(),
            sink,
        ));
        runtime.begin_session(SessionId::from_value(1)).await;
        runtime
            .push_chunk(chunk(AudioSource::SystemOutput))
            .await
            .unwrap();
        let old_revision = runtime.configuration_revision();

        let second = Arc::new(
            FakeTranscriptionProvider::new(FakeBehavior::EmitsFinal {
                text: "provider novo".into(),
                partials: false,
            })
            .with_provider_id(TranscriptionProviderId::GoogleGemini),
        );
        let second_log = second.log();
        let settings = TranscriptionSettings {
            provider: TranscriptionProviderId::GoogleGemini,
            language: crate::transcription::settings::LanguageCode::Automatic,
            ..TranscriptionSettings::default()
        };
        runtime.reconfigure(second, settings.clone()).await;
        let stale = runtime
            .push_chunk_for_revision(chunk(AudioSource::SystemOutput), old_revision)
            .await;
        assert!(matches!(stale, Err(TranscriptionError::Cancelled)));
        assert!(second_log.sessions().is_empty());
        assert_eq!(runtime.stats().discarded_stale_configuration, 1);
        runtime
            .push_chunk(chunk(AudioSource::SystemOutput))
            .await
            .unwrap();

        assert_eq!(first_log.cancel_count(), 1);
        assert_eq!(second_log.sessions().len(), 1);
        assert_eq!(runtime.settings(), settings);
        assert_eq!(finals(&out).len(), 1);
        assert_eq!(finals(&out)[0].transcript.text, "provider novo");
        assert_eq!(
            finals(&out)[0].transcript.provider,
            TranscriptionProviderId::GoogleGemini
        );
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

    // -----------------------------------------------------------------------
    // Identidade por fluxo: o que impede um resultado de herdar a origem de outro.
    // -----------------------------------------------------------------------

    fn identity(
        session_id: SessionId,
        source: AudioSource,
        stream: CaptureStreamId,
        sequence: u64,
        captured_at: MonotonicTimestamp,
    ) -> PendingSegmentIdentity {
        PendingSegmentIdentity {
            session_id,
            segment_id: SegmentId::next(),
            source,
            capture_stream_id: stream,
            sequence_number: sequence,
            captured_at,
            enqueued_at: captured_at,
        }
    }

    fn register(pending: &mut PendingSegmentTimings, identity: PendingSegmentIdentity) {
        pending
            .entry(identity.stream_key())
            .or_default()
            .push_back(identity);
    }

    /// O cenário que uma fila global quebraria: as duas fontes esperando resultado ao mesmo
    /// tempo, e o resultado do microfone chegando primeiro.
    #[test]
    fn a_microphone_result_never_consumes_the_system_output_identity() {
        let session = SessionId::from_value(1);
        let mut pending = PendingSegmentTimings::new();
        let mic_stream = CaptureStreamId::next();
        let system_stream = CaptureStreamId::next();

        // A saída de sistema entrou na fila **antes** — é a mais antiga, e seria a escolhida
        // por qualquer desempate por ordem de chegada global.
        let remote = identity(
            session,
            AudioSource::SystemOutput,
            system_stream,
            1,
            MonotonicTimestamp::now(),
        );
        let mine = identity(
            session,
            AudioSource::Microphone,
            mic_stream,
            1,
            MonotonicTimestamp::now(),
        );
        register(&mut pending, remote);
        register(&mut pending, mine);

        let (resolved, _) =
            resolve_pending_identity(&mut pending, session, AudioSource::Microphone, None)
                .expect("o microfone encontra a própria identidade");
        assert_eq!(resolved.segment_id, mine.segment_id);
        assert_eq!(resolved.source, AudioSource::Microphone);
        assert_eq!(resolved.capture_stream_id, mic_stream);

        // E a identidade da outra fonte continua intacta, esperando o resultado dela.
        let (still_there, _) =
            resolve_pending_identity(&mut pending, session, AudioSource::SystemOutput, None)
                .expect("a saída de sistema não foi consumida por outra fonte");
        assert_eq!(still_there.segment_id, remote.segment_id);
    }

    #[test]
    fn a_system_output_result_never_consumes_the_microphone_identity() {
        let session = SessionId::from_value(1);
        let mut pending = PendingSegmentTimings::new();
        let mine = identity(
            session,
            AudioSource::Microphone,
            CaptureStreamId::next(),
            1,
            MonotonicTimestamp::now(),
        );
        register(&mut pending, mine);

        assert!(
            resolve_pending_identity(&mut pending, session, AudioSource::SystemOutput, None)
                .is_none(),
            "sem segmento pendente da própria fonte, não se toma emprestado o da outra"
        );
    }

    /// Callbacks assíncronos podem chegar fora de ordem; a identidade é casada por
    /// `segment_id`, então a ordem não muda a origem de nada.
    #[test]
    fn out_of_order_callbacks_keep_each_result_with_its_own_origin() {
        let session = SessionId::from_value(1);
        let mut pending = PendingSegmentTimings::new();
        let stream = CaptureStreamId::next();
        let first = identity(
            session,
            AudioSource::SystemOutput,
            stream,
            1,
            MonotonicTimestamp::now(),
        );
        let second = identity(
            session,
            AudioSource::SystemOutput,
            stream,
            2,
            MonotonicTimestamp::now(),
        );
        register(&mut pending, first);
        register(&mut pending, second);

        // O segundo segmento termina primeiro.
        let (resolved, resolution) = resolve_pending_identity(
            &mut pending,
            session,
            AudioSource::SystemOutput,
            Some(second.segment_id),
        )
        .expect("casou pelo id");
        assert_eq!(resolution, IdentityResolution::BySegmentId);
        assert_eq!(resolved.sequence_number, 2);
        assert_eq!(resolved.source, AudioSource::SystemOutput);

        let (resolved, _) = resolve_pending_identity(
            &mut pending,
            session,
            AudioSource::SystemOutput,
            Some(first.segment_id),
        )
        .expect("casou pelo id");
        assert_eq!(resolved.sequence_number, 1);
    }

    /// Duas transcrições simultâneas, uma por fonte: cada resultado sai com o próprio
    /// `segment_id`, `capture_stream_id` e `sequence_number`.
    #[test]
    fn two_simultaneous_transcriptions_preserve_their_own_identifiers() {
        let session = SessionId::from_value(1);
        let mut pending = PendingSegmentTimings::new();
        let mic_stream = CaptureStreamId::next();
        let system_stream = CaptureStreamId::next();
        let mine = identity(
            session,
            AudioSource::Microphone,
            mic_stream,
            10,
            MonotonicTimestamp::now(),
        );
        let remote = identity(
            session,
            AudioSource::SystemOutput,
            system_stream,
            20,
            MonotonicTimestamp::now(),
        );
        register(&mut pending, mine);
        register(&mut pending, remote);

        let (a, _) = resolve_pending_identity(
            &mut pending,
            session,
            AudioSource::SystemOutput,
            Some(remote.segment_id),
        )
        .unwrap();
        let (b, _) = resolve_pending_identity(
            &mut pending,
            session,
            AudioSource::Microphone,
            Some(mine.segment_id),
        )
        .unwrap();

        assert_eq!(a.sequence_number, 20);
        assert_eq!(a.capture_stream_id, system_stream);
        assert_eq!(b.sequence_number, 10);
        assert_eq!(b.capture_stream_id, mic_stream);
        assert_ne!(a.segment_id, b.segment_id);
    }

    /// Dois fluxos da **mesma** fonte (troca de dispositivo) não se misturam por ordem.
    #[test]
    fn two_capture_streams_of_the_same_source_do_not_share_a_queue() {
        let session = SessionId::from_value(1);
        let mut pending = PendingSegmentTimings::new();
        let old_stream = CaptureStreamId::next();
        let new_stream = CaptureStreamId::next();
        let old = identity(
            session,
            AudioSource::Microphone,
            old_stream,
            1,
            MonotonicTimestamp::now(),
        );
        let new = identity(
            session,
            AudioSource::Microphone,
            new_stream,
            1,
            MonotonicTimestamp::now(),
        );
        register(&mut pending, old);
        register(&mut pending, new);

        assert_eq!(
            pending.len(),
            2,
            "uma fila por fluxo, não uma fila por fonte"
        );
        let (resolved, _) = resolve_pending_identity(
            &mut pending,
            session,
            AudioSource::Microphone,
            Some(new.segment_id),
        )
        .unwrap();
        assert_eq!(resolved.capture_stream_id, new_stream);
    }

    /// Sessão nova nunca casa com envelope de sessão anterior — nem pelo fallback por ordem,
    /// que é o caminho mais permissivo que existe aqui.
    #[test]
    fn a_new_session_never_matches_a_pending_identity_of_the_previous_one() {
        let previous = SessionId::from_value(1);
        let current = SessionId::from_value(2);
        let mut pending = PendingSegmentTimings::new();
        register(
            &mut pending,
            identity(
                previous,
                AudioSource::SystemOutput,
                CaptureStreamId::next(),
                1,
                MonotonicTimestamp::now(),
            ),
        );

        assert!(
            resolve_pending_identity(&mut pending, current, AudioSource::SystemOutput, None)
                .is_none()
        );
    }
}
