//! Eventos normalizados de transcrição — a forma **única** em que todo provider reporta
//! resultado, independentemente de ser batch (whisper.cpp local) ou streaming (Realtime,
//! Live). Um consumidor a jusante (runtime, normalização, timeline) nunca precisa saber
//! qual backend produziu o texto.
//!
//! Todo payload carrega a identidade completa do resultado: `session_id` (sessão de
//! conversa), `transcription_session_id` (sessão de transcrição daquela fonte), `source`,
//! `provider`, `language`, `started_at`/`ended_at`, `confidence`, `is_final` e
//! `provider_event_id`. Sem essa identidade, um resultado atrasado é indistinguível de um
//! resultado atual e só poderia ser descartado no frontend — tarde demais, porque a
//! timeline já teria virado uma utterance e a geração de resposta já teria disparado.

use serde::Serialize;

use crate::audio::segment::AudioTimestamp;
use crate::audio::types::AudioSource;
use crate::conversation::SessionId;
use crate::transcription::provider::TranscriptionProviderId;
use crate::transcription::session::TranscriptionSessionId;

/// Identificador do evento **do ponto de vista do provider**. Providers de streaming
/// costumam expor um id próprio por item (`item_id` da API Realtime, por exemplo); um
/// backend batch não expõe nada disso, então o adaptador sintetiza um id determinístico a
/// partir do que ele tem (segmento + sessão). Em ambos os casos o contrato é o mesmo: dois
/// eventos distintos nunca compartilham `provider_event_id` dentro da mesma sessão de
/// transcrição, o que permite deduplicar reentregas.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct ProviderEventId(pub String);

impl ProviderEventId {
    pub fn new(value: impl Into<String>) -> Self {
        ProviderEventId(value.into())
    }
}

impl std::fmt::Display for ProviderEventId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Campos comuns a resultados parciais e finais. Um único struct em vez de dois conjuntos
/// paralelos de campos: um parcial e o final da mesma fala precisam ser comparáveis campo a
/// campo, e duplicar a definição garantiria que eles divergissem com o tempo.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TranscriptPayload {
    pub session_id: SessionId,
    pub transcription_session_id: TranscriptionSessionId,
    pub source: AudioSource,
    pub provider: TranscriptionProviderId,
    /// Idioma efetivamente usado/detectado, quando o provider reporta um.
    pub language: Option<String>,
    pub text: String,
    pub started_at: AudioTimestamp,
    pub ended_at: AudioTimestamp,
    /// Confiança reportada pelo provider, em `0.0..=1.0`. `None` quando o backend não
    /// expõe confiança — nunca um valor inventado como `1.0`.
    pub confidence: Option<f32>,
    pub is_final: bool,
    pub provider_event_id: ProviderEventId,
    /// Segmento de áudio que originou este resultado, quando o backend é alimentado por
    /// segmentos já recortados. `None` para streaming puro, em que não existe um segmento
    /// correspondente — nesse caso a timeline sintetiza um id próprio.
    pub segment_id: Option<crate::audio::segment::SegmentId>,
    /// Tempo de inferência medido pelo provider, quando mensurável.
    pub processing_time_ms: Option<u64>,
}

/// Resultado ainda sujeito a revisão pelo provider. Nunca entra na timeline: um parcial é
/// para feedback visual imediato, e promovê-lo a segmento produziria fala duplicada quando
/// o final chegasse.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct PartialTranscript(pub TranscriptPayload);

/// Resultado estável de um trecho de fala. É o único que vira `TranscriptSegment`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct FinalTranscript(pub TranscriptPayload);

impl std::ops::Deref for PartialTranscript {
    type Target = TranscriptPayload;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::Deref for FinalTranscript {
    type Target = TranscriptPayload;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Fronteira de fala detectada pelo provider (VAD do próprio backend, ou o recorte que o
/// pipeline local já faz). Serve à telemetria de latência: `speech_ended` é o marco a
/// partir do qual "silêncio → resposta visível" é medido.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SpeechBoundary {
    pub session_id: SessionId,
    pub transcription_session_id: TranscriptionSessionId,
    pub source: AudioSource,
    pub provider: TranscriptionProviderId,
    pub at: AudioTimestamp,
    pub provider_event_id: ProviderEventId,
}

/// Falha reportada por um provider, já atribuída a uma sessão/fonte. `recoverable`
/// distingue o que permite continuar (uma inferência que falhou num segmento) do que
/// derruba a sessão (credencial inválida, conexão perdida sem retry).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TranscriptionErrorEvent {
    pub session_id: SessionId,
    pub transcription_session_id: TranscriptionSessionId,
    pub source: AudioSource,
    pub provider: TranscriptionProviderId,
    pub message: String,
    pub recoverable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptionEvent {
    Partial(PartialTranscript),
    Final(FinalTranscript),
    SpeechStarted(SpeechBoundary),
    SpeechEnded(SpeechBoundary),
    Error(TranscriptionErrorEvent),
}

impl TranscriptionEvent {
    pub fn session_id(&self) -> SessionId {
        match self {
            TranscriptionEvent::Partial(p) => p.session_id,
            TranscriptionEvent::Final(f) => f.session_id,
            TranscriptionEvent::SpeechStarted(b) | TranscriptionEvent::SpeechEnded(b) => {
                b.session_id
            }
            TranscriptionEvent::Error(e) => e.session_id,
        }
    }

    pub fn transcription_session_id(&self) -> TranscriptionSessionId {
        match self {
            TranscriptionEvent::Partial(p) => p.transcription_session_id,
            TranscriptionEvent::Final(f) => f.transcription_session_id,
            TranscriptionEvent::SpeechStarted(b) | TranscriptionEvent::SpeechEnded(b) => {
                b.transcription_session_id
            }
            TranscriptionEvent::Error(e) => e.transcription_session_id,
        }
    }

    pub fn source(&self) -> AudioSource {
        match self {
            TranscriptionEvent::Partial(p) => p.source,
            TranscriptionEvent::Final(f) => f.source,
            TranscriptionEvent::SpeechStarted(b) | TranscriptionEvent::SpeechEnded(b) => b.source,
            TranscriptionEvent::Error(e) => e.source,
        }
    }

    /// `None` para eventos que não são resultados de texto (`Error`), já que dedupe por id
    /// só faz sentido para resultados.
    pub fn provider_event_id(&self) -> Option<&ProviderEventId> {
        match self {
            TranscriptionEvent::Partial(p) => Some(&p.provider_event_id),
            TranscriptionEvent::Final(f) => Some(&f.provider_event_id),
            TranscriptionEvent::SpeechStarted(b) | TranscriptionEvent::SpeechEnded(b) => {
                Some(&b.provider_event_id)
            }
            TranscriptionEvent::Error(_) => None,
        }
    }
}
