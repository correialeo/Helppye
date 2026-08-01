//! Envelope causal da transcrição.
//!
//! O contrato que este módulo estabelece é curto e vale a pena enunciá-lo antes do código:
//! **o transcritor não decide de onde veio o áudio**. Ele recebe um envelope
//! (`TranscriptionWorkItem`), devolve texto, e a origem que sai
//! (`TranscriptionResultEnvelope`) é copiada do envelope que entrou — nunca lida do que o
//! provider afirmou.
//!
//! Antes disso, a origem viajava só como um campo de `AudioSegment` que era re-declarado a
//! cada camada (`AudioChunk.source`, `Transcript.source`, `FinalTranscript.source`,
//! `TranscriptSegment.source`). Cada re-declaração é um lugar onde a fonte pode divergir do
//! que foi capturado, e nenhuma delas tinha como perceber a divergência: eram todas
//! atribuições, não comparações.
//!
//! A identidade completa de um segmento é `session_id + capture_stream_id + segment_id`, com
//! `sequence_number` como posição dentro do fluxo. `AudioSource` sozinha **não** é
//! identidade: dois fluxos da mesma fonte podem coexistir por um instante durante uma troca
//! de dispositivo, e um resultado atrasado do fluxo antigo não pode ser casado com um
//! segmento do novo.

use std::time::Instant;

use crate::audio::segment::{AudioSegment, SegmentId};
use crate::audio::types::{AudioSource, CaptureStreamId};
use crate::conversation::SessionId;

/// Instante monotônico do processo. Wrapper fino sobre `Instant` pelo mesmo motivo que a
/// telemetria usa `Instant` e não epoch: uma reunião longa é exatamente o cenário em que um
/// relógio de parede ajustado produziria durações negativas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct MonotonicTimestamp(Instant);

impl MonotonicTimestamp {
    pub fn now() -> Self {
        MonotonicTimestamp(Instant::now())
    }

    pub fn from_instant(instant: Instant) -> Self {
        MonotonicTimestamp(instant)
    }

    pub fn as_instant(self) -> Instant {
        self.0
    }

    pub fn elapsed_ms(self) -> u64 {
        self.0.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
    }
}

/// Chave da fila de identidades pendentes de um fluxo de transcrição.
///
/// As três partes são necessárias e nenhuma é redundante: `session_id` impede que um
/// resultado de uma sessão encerrada case com um segmento da sessão nova; `source` é o que
/// impede que um resultado do microfone consuma a identidade de um segmento da saída de
/// sistema (e vice-versa); `capture_stream_id` impede o mesmo entre dois fluxos da *mesma*
/// fonte durante uma troca de dispositivo.
///
/// Uma fila única e global — que era o risco real aqui — casaria resultado com identidade
/// por ordem de chegada entre fontes distintas, e transcrição é assíncrona: basta o
/// microfone terminar antes para a fala da outra pessoa herdar a identidade errada.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TranscriptionStreamKey {
    pub session_id: SessionId,
    pub source: AudioSource,
    pub capture_stream_id: CaptureStreamId,
}

impl TranscriptionStreamKey {
    pub fn new(
        session_id: SessionId,
        source: AudioSource,
        capture_stream_id: CaptureStreamId,
    ) -> Self {
        TranscriptionStreamKey {
            session_id,
            source,
            capture_stream_id,
        }
    }
}

/// O que a transcrição guarda sobre um segmento enquanto espera o resultado dele. É esta
/// cópia — não o que o provider devolver — que define a origem do texto que sai.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingSegmentIdentity {
    pub session_id: SessionId,
    pub segment_id: SegmentId,
    pub source: AudioSource,
    pub capture_stream_id: CaptureStreamId,
    pub sequence_number: u64,
    pub captured_at: MonotonicTimestamp,
    pub enqueued_at: MonotonicTimestamp,
}

impl PendingSegmentIdentity {
    pub fn stream_key(&self) -> TranscriptionStreamKey {
        TranscriptionStreamKey::new(self.session_id, self.source, self.capture_stream_id)
    }
}

/// Unidade de trabalho entregue à camada de transcrição: áudio **mais** a identidade causal
/// de quem o produziu. O áudio é o payload; a identidade é o contrato.
#[derive(Debug, Clone)]
pub struct TranscriptionWorkItem {
    pub session_id: SessionId,
    pub segment_id: SegmentId,
    pub source: AudioSource,
    pub capture_stream_id: CaptureStreamId,
    pub sequence_number: u64,
    pub captured_at: MonotonicTimestamp,
    pub enqueued_at: MonotonicTimestamp,
    pub audio: AudioSegment,
}

impl TranscriptionWorkItem {
    /// Constrói o item a partir do segmento recém-produzido pela captura. `captured_at` é o
    /// instante em que a fala terminou (o segmento ficou pronto), `enqueued_at` o instante
    /// em que entrou na fila — os dois separados porque a diferença entre eles é backlog de
    /// fila, e confundi-los esconderia justamente a latência que se quer medir.
    pub fn from_segment(
        session_id: SessionId,
        audio: AudioSegment,
        captured_at: MonotonicTimestamp,
        enqueued_at: MonotonicTimestamp,
    ) -> Self {
        TranscriptionWorkItem {
            session_id,
            segment_id: audio.id,
            source: audio.source,
            capture_stream_id: audio.capture_stream_id,
            sequence_number: audio.sequence_number,
            captured_at,
            enqueued_at,
            audio,
        }
    }

    pub fn identity(&self) -> PendingSegmentIdentity {
        PendingSegmentIdentity {
            session_id: self.session_id,
            segment_id: self.segment_id,
            source: self.source,
            capture_stream_id: self.capture_stream_id,
            sequence_number: self.sequence_number,
            captured_at: self.captured_at,
            enqueued_at: self.enqueued_at,
        }
    }

    pub fn stream_key(&self) -> TranscriptionStreamKey {
        TranscriptionStreamKey::new(self.session_id, self.source, self.capture_stream_id)
    }
}

/// Resultado de transcrição já casado com a identidade do segmento que o originou.
///
/// `source`, `capture_stream_id` e `sequence_number` vêm da `PendingSegmentIdentity`
/// guardada no envio — **não** do que o provider reportou. O que o provider reportou é
/// comparado com isto e uma divergência vira `SourceIntegrityError`, não uma correção
/// silenciosa.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptionResultEnvelope {
    pub session_id: SessionId,
    pub segment_id: SegmentId,
    pub source: AudioSource,
    pub capture_stream_id: CaptureStreamId,
    pub sequence_number: u64,
    pub raw_text: String,
    pub normalized_text: String,
}

impl TranscriptionResultEnvelope {
    pub fn from_identity(
        identity: PendingSegmentIdentity,
        raw_text: String,
        normalized_text: String,
    ) -> Self {
        TranscriptionResultEnvelope {
            session_id: identity.session_id,
            segment_id: identity.segment_id,
            source: identity.source,
            capture_stream_id: identity.capture_stream_id,
            sequence_number: identity.sequence_number,
            raw_text,
            normalized_text,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::segment::AudioTimestamp;

    fn segment(source: AudioSource, stream: CaptureStreamId, sequence: u64) -> AudioSegment {
        AudioSegment::new(
            source,
            vec![0.0; 1_600],
            16_000,
            AudioTimestamp(0),
            AudioTimestamp(100),
        )
        .in_stream(stream, sequence)
    }

    #[test]
    fn work_item_copies_identity_from_the_segment_it_wraps() {
        let stream = CaptureStreamId::next();
        let audio = segment(AudioSource::SystemOutput, stream, 7);
        let segment_id = audio.id;
        let item = TranscriptionWorkItem::from_segment(
            SessionId::from_value(3),
            audio,
            MonotonicTimestamp::now(),
            MonotonicTimestamp::now(),
        );

        assert_eq!(item.source, AudioSource::SystemOutput);
        assert_eq!(item.capture_stream_id, stream);
        assert_eq!(item.sequence_number, 7);
        assert_eq!(item.segment_id, segment_id);
    }

    #[test]
    fn stream_keys_of_different_sources_never_collide() {
        let session = SessionId::from_value(1);
        let stream = CaptureStreamId::next();
        let mic = TranscriptionStreamKey::new(session, AudioSource::Microphone, stream);
        let system = TranscriptionStreamKey::new(session, AudioSource::SystemOutput, stream);
        assert_ne!(mic, system);
    }

    #[test]
    fn stream_keys_of_different_capture_streams_never_collide() {
        let session = SessionId::from_value(1);
        let first =
            TranscriptionStreamKey::new(session, AudioSource::Microphone, CaptureStreamId::next());
        let second =
            TranscriptionStreamKey::new(session, AudioSource::Microphone, CaptureStreamId::next());
        assert_ne!(first, second);
    }

    #[test]
    fn the_envelope_takes_its_origin_from_the_identity_not_from_the_text() {
        let identity = PendingSegmentIdentity {
            session_id: SessionId::from_value(2),
            segment_id: SegmentId::next(),
            source: AudioSource::SystemOutput,
            capture_stream_id: CaptureStreamId::next(),
            sequence_number: 1,
            captured_at: MonotonicTimestamp::now(),
            enqueued_at: MonotonicTimestamp::now(),
        };
        let envelope = TranscriptionResultEnvelope::from_identity(
            identity,
            "bruto".into(),
            "normalizado".into(),
        );
        assert_eq!(envelope.source, AudioSource::SystemOutput);
        assert_eq!(envelope.segment_id, identity.segment_id);
        assert_eq!(envelope.capture_stream_id, identity.capture_stream_id);
    }
}
