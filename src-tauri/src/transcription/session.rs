//! Identidade e contrato de uma **sessão de transcrição**: o recorte de tempo em que um
//! provider está transcrevendo *uma* fonte de áudio dentro de *uma* sessão de conversa.
//!
//! Antes desta camada, transcrição não tinha identidade nenhuma: um `AudioSegment` entrava
//! na fila e um `Transcript` saía, sem nada que dissesse a qual sessão de conversa aquele
//! resultado pertencia. Um resultado que chegasse atrasado (inferência lenta, resposta de
//! rede pendente) era indistinguível de um resultado da sessão atual e entrava na timeline
//! como se fosse fala nova. `TranscriptionSessionId` é o que torna esse descarte possível
//! **no backend** — ver `runtime.rs`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;

use crate::audio::segment::{AudioTimestamp, SegmentId};
use crate::audio::types::AudioSource;
use crate::conversation::SessionId;
use crate::transcription::error::TranscriptionError;
use crate::transcription::events::TranscriptionEvent;
use crate::transcription::types::TranscriptionLanguage;

/// Identificador monotônico de uma sessão de transcrição. Único por processo: um contador
/// global, e não um contador por fonte, para que um id nunca seja ambíguo entre microfone e
/// saída de sistema em logs, diagnósticos ou comparações de evento atrasado.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct TranscriptionSessionId(pub u64);

static NEXT_TRANSCRIPTION_SESSION_ID: AtomicU64 = AtomicU64::new(1);

impl TranscriptionSessionId {
    pub fn next() -> Self {
        TranscriptionSessionId(NEXT_TRANSCRIPTION_SESSION_ID.fetch_add(1, Ordering::SeqCst))
    }
}

impl std::fmt::Display for TranscriptionSessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Um bloco de áudio entregue a uma sessão de transcrição. Mono f32 na taxa indicada — a
/// mesma forma que o pipeline de captura já produz (`CaptureConfig` reamostra toda fonte
/// para 16 kHz mono antes daqui).
///
/// `source` viaja junto com as amostras de propósito: nenhum ponto desta camada pode
/// misturar microfone com saída de sistema, e carregar a fonte no próprio dado torna
/// impossível uma sessão receber áudio da outra fonte sem que a checagem em
/// `TranscriptionRuntime` perceba.
#[derive(Debug, Clone)]
pub struct AudioChunk {
    pub source: AudioSource,
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub started_at: AudioTimestamp,
    pub ended_at: AudioTimestamp,
    /// Presente quando o chunk veio de um `AudioSegment` já recortado pelo VAD (caminho
    /// atual, batch). Um backend de streaming puro entrega chunks contínuos sem segment id.
    pub segment_id: Option<SegmentId>,
}

impl AudioChunk {
    pub fn from_segment(segment: crate::audio::segment::AudioSegment) -> Self {
        AudioChunk {
            source: segment.source,
            samples: segment.samples,
            sample_rate: segment.sample_rate,
            started_at: segment.started_at,
            ended_at: segment.ended_at,
            segment_id: Some(segment.id),
        }
    }

    pub fn duration_ms(&self) -> u64 {
        self.ended_at.0.saturating_sub(self.started_at.0)
    }
}

/// Canal por onde um provider publica `TranscriptionEvent`s. Um `Fn` compartilhado em vez
/// de um `mpsc::Sender` porque providers de streaming emitem de dentro de tasks próprias e
/// o runtime precisa filtrar cada evento de forma síncrona antes de qualquer coisa a
/// jusante — filtrar num consumidor separado reintroduziria a janela em que um evento
/// obsoleto já está em trânsito.
pub type TranscriptionEventSink = Arc<dyn Fn(TranscriptionEvent) + Send + Sync>;

/// Tudo que um provider precisa para abrir uma sessão. A identidade (`session_id` +
/// `transcription_session_id` + `source`) é fornecida pelo runtime, nunca inventada pelo
/// provider: é ela que o runtime usa depois para decidir se um evento ainda é válido.
#[derive(Clone)]
pub struct TranscriptionSessionContext {
    pub session_id: SessionId,
    pub transcription_session_id: TranscriptionSessionId,
    pub source: AudioSource,
    pub language: TranscriptionLanguage,
    /// Modelo pedido pelo usuário, quando o provider aceita escolha de modelo.
    pub model: Option<String>,
    pub sink: TranscriptionEventSink,
}

impl std::fmt::Debug for TranscriptionSessionContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TranscriptionSessionContext")
            .field("session_id", &self.session_id)
            .field("transcription_session_id", &self.transcription_session_id)
            .field("source", &self.source)
            .field("language", &self.language)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl TranscriptionSessionContext {
    pub fn emit(&self, event: TranscriptionEvent) {
        (self.sink)(event);
    }
}

/// Sessão viva de transcrição de uma fonte. Implementações **não** decidem sozinhas quando
/// parar: `finish` (drenar o que resta e encerrar) e `cancel` (descartar sem produzir mais
/// nada) são chamados pelo runtime, que é quem conhece a fronteira de sessão de conversa.
///
/// Depois de `finish` ou `cancel`, um `push_audio` deve falhar com
/// `TranscriptionError::SessionClosed` em vez de aceitar áudio silenciosamente — aceitar
/// áudio numa sessão encerrada é exatamente o vazamento entre sessões que esta camada
/// existe para impedir.
#[async_trait]
pub trait TranscriptionSession: Send {
    async fn push_audio(&mut self, chunk: AudioChunk) -> Result<(), TranscriptionError>;
    async fn finish(&mut self) -> Result<(), TranscriptionError>;
    async fn cancel(&mut self) -> Result<(), TranscriptionError>;
}
