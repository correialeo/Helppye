//! `WhisperLocalTranscriptionProvider`: adapta o engine batch local (`SegmentTranscriber`,
//! hoje `WhisperCppProvider`) ao contrato de sessão de `TranscriptionProvider`.
//!
//! Nenhum comportamento de transcrição muda aqui — o mesmo modelo, o mesmo
//! `spawn_blocking`, a mesma limpeza de anotação sem-fala. O que este arquivo acrescenta é
//! **identidade e ciclo de vida**: cada chunk vira um resultado final atribuído a uma
//! sessão de conversa, a uma sessão de transcrição e a uma fonte, com um
//! `provider_event_id` determinístico. É isso que permite ao runtime descartar no backend
//! um resultado que chegou depois da sessão acabar.
//!
//! Whisper.cpp não produz resultados parciais e não é streaming: `capabilities()` declara
//! isso em vez de simular parciais recortando o texto final, o que só criaria a ilusão de
//! progresso.

use std::sync::Arc;

use async_trait::async_trait;

use crate::transcription::error::TranscriptionError;
use crate::transcription::events::{
    FinalTranscript, ProviderEventId, SpeechBoundary, TranscriptPayload, TranscriptionErrorEvent,
    TranscriptionEvent,
};
use crate::transcription::provider::{
    TranscriptionCapabilities, TranscriptionProvider, TranscriptionProviderId,
};
use crate::transcription::segment_transcriber::SegmentTranscriber;
use crate::transcription::session::{
    AudioChunk, TranscriptionSession, TranscriptionSessionContext,
};

pub struct WhisperLocalTranscriptionProvider {
    transcriber: Arc<dyn SegmentTranscriber>,
}

impl WhisperLocalTranscriptionProvider {
    pub fn new(transcriber: Arc<dyn SegmentTranscriber>) -> Self {
        WhisperLocalTranscriptionProvider { transcriber }
    }
}

#[async_trait]
impl TranscriptionProvider for WhisperLocalTranscriptionProvider {
    fn id(&self) -> TranscriptionProviderId {
        TranscriptionProviderId::WhisperLocal
    }

    fn capabilities(&self) -> TranscriptionCapabilities {
        TranscriptionCapabilities {
            local: true,
            streaming: false,
            partial_results: false,
            speaker_source_preserved: true,
            language_selection: true,
            automatic_language_detection: true,
            requires_credentials: false,
        }
    }

    async fn start_session(
        &self,
        context: TranscriptionSessionContext,
    ) -> Result<Box<dyn TranscriptionSession>, TranscriptionError> {
        Ok(Box::new(WhisperLocalSession {
            transcriber: Arc::clone(&self.transcriber),
            context,
            closed: false,
            chunk_sequence: 0,
        }))
    }
}

struct WhisperLocalSession {
    transcriber: Arc<dyn SegmentTranscriber>,
    context: TranscriptionSessionContext,
    closed: bool,
    chunk_sequence: u64,
}

impl WhisperLocalSession {
    /// Determinístico e único dentro da sessão de transcrição: o `SegmentId` quando o chunk
    /// veio do segmentador, um contador quando não veio. Nunca aleatório — um id
    /// reproduzível é o que torna a deduplicação verificável em teste.
    fn event_id(&self, chunk: &AudioChunk, sequence: u64) -> ProviderEventId {
        match chunk.segment_id {
            Some(segment_id) => ProviderEventId::new(format!(
                "whisper-local:{}:segment:{}",
                self.context.transcription_session_id,
                segment_id.value()
            )),
            None => ProviderEventId::new(format!(
                "whisper-local:{}:chunk:{sequence}",
                self.context.transcription_session_id
            )),
        }
    }

    fn boundary(&self, at: crate::audio::segment::AudioTimestamp, kind: &str) -> SpeechBoundary {
        SpeechBoundary {
            session_id: self.context.session_id,
            transcription_session_id: self.context.transcription_session_id,
            source: self.context.source,
            provider: TranscriptionProviderId::WhisperLocal,
            at,
            provider_event_id: ProviderEventId::new(format!(
                "whisper-local:{}:{kind}:{}",
                self.context.transcription_session_id, at.0
            )),
        }
    }
}

#[async_trait]
impl TranscriptionSession for WhisperLocalSession {
    async fn push_audio(&mut self, chunk: AudioChunk) -> Result<(), TranscriptionError> {
        if self.closed {
            return Err(TranscriptionError::SessionClosed);
        }
        if chunk.source != self.context.source {
            // Uma sessão é de uma fonte só. Aceitar áudio da outra aqui misturaria
            // microfone com saída de sistema no mesmo fluxo de transcrição — a fala do
            // usuário viraria fala da outra pessoa e dispararia geração de resposta.
            return Err(TranscriptionError::SourceMismatch {
                expected: self.context.source,
                received: chunk.source,
            });
        }

        self.chunk_sequence += 1;
        let sequence = self.chunk_sequence;
        let provider_event_id = self.event_id(&chunk, sequence);

        // As duas fronteiras são emitidas **antes** da inferência, e não em volta dela. Um
        // backend batch nunca vê fala acontecendo: o chunk que chega aqui já é um trecho
        // recortado pelo VAD a montante, ou seja, a fala já começou e já terminou. Emitir
        // `SpeechEnded` depois de transcrever dataria o fim da fala pelo fim da inferência
        // e faria a telemetria medir "fim da fala → transcrição final" como ~0 ms,
        // escondendo exatamente o tempo que se quer observar.
        self.context.emit(TranscriptionEvent::SpeechStarted(
            self.boundary(chunk.started_at, "speech-started"),
        ));
        self.context.emit(TranscriptionEvent::SpeechEnded(
            self.boundary(chunk.ended_at, "speech-ended"),
        ));

        let segment = chunk_to_segment(&chunk);
        let result = self.transcriber.transcribe(segment).await;

        match result {
            Ok(transcript) => {
                self.context.emit(TranscriptionEvent::Final(FinalTranscript(
                    TranscriptPayload {
                        session_id: self.context.session_id,
                        transcription_session_id: self.context.transcription_session_id,
                        source: transcript.source,
                        provider: TranscriptionProviderId::WhisperLocal,
                        language: transcript.language,
                        text: transcript.text,
                        started_at: transcript.started_at,
                        ended_at: transcript.ended_at,
                        // whisper.cpp não expõe confiança agregada por segmento pela
                        // API que usamos; reportar `None` é honesto, inventar 1.0 não.
                        confidence: None,
                        is_final: true,
                        provider_event_id,
                        segment_id: Some(transcript.segment_id),
                        processing_time_ms: Some(transcript.processing_time_ms),
                    },
                )));
                Ok(())
            }
            Err(e) => {
                self.context
                    .emit(TranscriptionEvent::Error(TranscriptionErrorEvent {
                        session_id: self.context.session_id,
                        transcription_session_id: self.context.transcription_session_id,
                        source: self.context.source,
                        provider: TranscriptionProviderId::WhisperLocal,
                        message: e.to_string(),
                        // Uma inferência que falhou num chunk não invalida a sessão: o
                        // próximo chunk pode transcrever normalmente.
                        recoverable: !matches!(e, TranscriptionError::NotConfigured),
                    }));
                Err(e)
            }
        }
    }

    async fn finish(&mut self) -> Result<(), TranscriptionError> {
        // Batch: não há buffer interno para drenar — todo chunk já foi transcrito de forma
        // síncrona dentro de `push_audio`. Encerrar é só fechar a porta para áudio novo.
        self.closed = true;
        Ok(())
    }

    async fn cancel(&mut self) -> Result<(), TranscriptionError> {
        self.closed = true;
        Ok(())
    }
}

fn chunk_to_segment(chunk: &AudioChunk) -> crate::audio::segment::AudioSegment {
    let mut segment = crate::audio::segment::AudioSegment::new(
        chunk.source,
        chunk.samples.clone(),
        chunk.sample_rate,
        chunk.started_at,
        chunk.ended_at,
    );
    if let Some(id) = chunk.segment_id {
        segment.id = id;
    }
    segment
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::segment::{AudioSegment, AudioTimestamp};
    use crate::audio::types::AudioSource;
    use crate::conversation::SessionId;
    use crate::transcription::session::TranscriptionSessionId;
    use crate::transcription::types::{ModelConfig, Transcript, TranscriptionLanguage};
    use std::sync::Mutex;

    struct StubTranscriber {
        text: String,
        fail: bool,
    }

    #[async_trait]
    impl SegmentTranscriber for StubTranscriber {
        async fn load(&self, _config: ModelConfig) -> Result<(), TranscriptionError> {
            Ok(())
        }

        async fn transcribe(
            &self,
            segment: AudioSegment,
        ) -> Result<Transcript, TranscriptionError> {
            if self.fail {
                return Err(TranscriptionError::InferenceFailed("stub".into()));
            }
            Ok(Transcript {
                segment_id: segment.id,
                source: segment.source,
                text: self.text.clone(),
                language: Some("pt".into()),
                started_at: segment.started_at,
                ended_at: segment.ended_at,
                processing_time_ms: 7,
            })
        }

        fn provider_name(&self) -> &'static str {
            "stub"
        }
    }

    fn collector() -> (
        Arc<Mutex<Vec<TranscriptionEvent>>>,
        crate::transcription::session::TranscriptionEventSink,
    ) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink_events = Arc::clone(&events);
        let sink: crate::transcription::session::TranscriptionEventSink =
            Arc::new(move |event| sink_events.lock().unwrap().push(event));
        (events, sink)
    }

    fn context(
        source: AudioSource,
        sink: crate::transcription::session::TranscriptionEventSink,
    ) -> TranscriptionSessionContext {
        TranscriptionSessionContext {
            session_id: SessionId::from_value(1),
            transcription_session_id: TranscriptionSessionId(42),
            source,
            language: TranscriptionLanguage::default(),
            model: Some("ggml-base.bin".into()),
            sink,
        }
    }

    fn chunk(source: AudioSource) -> AudioChunk {
        AudioChunk {
            source,
            capture_stream_id: crate::audio::types::CaptureStreamId::UNASSIGNED,
            sequence_number: 0,
            samples: vec![0.0; 1_600],
            sample_rate: 16_000,
            started_at: AudioTimestamp(0),
            ended_at: AudioTimestamp(100),
            segment_id: None,
        }
    }

    #[tokio::test]
    async fn capabilities_declare_local_batch_without_partials() {
        let provider = WhisperLocalTranscriptionProvider::new(Arc::new(StubTranscriber {
            text: "oi".into(),
            fail: false,
        }));
        let caps = provider.capabilities();
        assert!(caps.local);
        assert!(!caps.streaming);
        assert!(!caps.partial_results);
        assert!(caps.speaker_source_preserved);
        assert!(!caps.requires_credentials);
        assert_eq!(provider.id(), TranscriptionProviderId::WhisperLocal);
    }

    #[tokio::test]
    async fn push_audio_emits_a_final_transcript_carrying_full_identity() {
        let (events, sink) = collector();
        let provider = WhisperLocalTranscriptionProvider::new(Arc::new(StubTranscriber {
            text: "olá mundo".into(),
            fail: false,
        }));
        let mut session = provider
            .start_session(context(AudioSource::SystemOutput, sink))
            .await
            .unwrap();

        session
            .push_audio(chunk(AudioSource::SystemOutput))
            .await
            .unwrap();

        let events = events.lock().unwrap();
        let final_event = events
            .iter()
            .find_map(|e| match e {
                TranscriptionEvent::Final(f) => Some(f.clone()),
                _ => None,
            })
            .expect("um resultado final");
        assert_eq!(final_event.session_id, SessionId::from_value(1));
        assert_eq!(
            final_event.transcription_session_id,
            TranscriptionSessionId(42)
        );
        assert_eq!(final_event.source, AudioSource::SystemOutput);
        assert_eq!(final_event.provider, TranscriptionProviderId::WhisperLocal);
        assert_eq!(final_event.text, "olá mundo");
        assert!(final_event.is_final);
        assert_eq!(final_event.confidence, None);
        assert_eq!(final_event.processing_time_ms, Some(7));
    }

    #[tokio::test]
    async fn session_rejects_audio_from_the_other_source() {
        let (_events, sink) = collector();
        let provider = WhisperLocalTranscriptionProvider::new(Arc::new(StubTranscriber {
            text: "x".into(),
            fail: false,
        }));
        let mut session = provider
            .start_session(context(AudioSource::SystemOutput, sink))
            .await
            .unwrap();

        let err = session
            .push_audio(chunk(AudioSource::Microphone))
            .await
            .unwrap_err();
        assert!(matches!(err, TranscriptionError::SourceMismatch { .. }));
    }

    #[tokio::test]
    async fn closed_session_refuses_new_audio() {
        let (_events, sink) = collector();
        let provider = WhisperLocalTranscriptionProvider::new(Arc::new(StubTranscriber {
            text: "x".into(),
            fail: false,
        }));
        let mut session = provider
            .start_session(context(AudioSource::Microphone, sink))
            .await
            .unwrap();

        session.cancel().await.unwrap();
        let err = session
            .push_audio(chunk(AudioSource::Microphone))
            .await
            .unwrap_err();
        assert!(matches!(err, TranscriptionError::SessionClosed));
    }

    #[tokio::test]
    async fn inference_failure_becomes_a_recoverable_error_event() {
        let (events, sink) = collector();
        let provider = WhisperLocalTranscriptionProvider::new(Arc::new(StubTranscriber {
            text: String::new(),
            fail: true,
        }));
        let mut session = provider
            .start_session(context(AudioSource::SystemOutput, sink))
            .await
            .unwrap();

        assert!(session
            .push_audio(chunk(AudioSource::SystemOutput))
            .await
            .is_err());

        let events = events.lock().unwrap();
        let error = events
            .iter()
            .find_map(|e| match e {
                TranscriptionEvent::Error(err) => Some(err.clone()),
                _ => None,
            })
            .expect("um evento de erro");
        assert!(error.recoverable);
        assert_eq!(error.source, AudioSource::SystemOutput);
    }
}
