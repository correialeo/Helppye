//! Provider de transcrição controlado, **só para testes**. Nunca é registrado no registry
//! de produção (`registry::production`) e nunca aparece na UI.
//!
//! Existe porque a maior parte do que esta camada garante — descarte de evento obsoleto,
//! cancelamento na fronteira de sessão, dedupe por `provider_event_id`, recusa de áudio da
//! outra fonte — não é observável com o Whisper real: exigiria modelo baixado, áudio
//! gravado e tempo de inferência real, e ainda assim não permitiria roteirizar um resultado
//! que chega *depois* da sessão acabar. Com um provider roteirizável isso vira um teste
//! determinístico de milissegundos.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::audio::types::AudioSource;
use crate::transcription::error::TranscriptionError;
use crate::transcription::events::{
    FinalTranscript, PartialTranscript, ProviderEventId, SpeechBoundary, TranscriptPayload,
    TranscriptionErrorEvent, TranscriptionEvent,
};
use crate::transcription::provider::{
    TranscriptionCapabilities, TranscriptionProvider, TranscriptionProviderId,
};
use crate::transcription::session::{
    AudioChunk, TranscriptionSession, TranscriptionSessionContext,
};

/// O que o provider faz a cada `push_audio`.
#[derive(Debug, Clone)]
pub enum FakeBehavior {
    /// Emite um parcial (se `partials`) e depois um final com este texto.
    EmitsFinal { text: String, partials: bool },
    /// Emite um final só depois de `delay`, para exercitar o caso do resultado que chega
    /// atrasado — inclusive depois da sessão de conversa ter mudado.
    EmitsFinalAfter {
        text: String,
        delay: std::time::Duration,
    },
    /// Emite **o mesmo** `provider_event_id` duas vezes, para exercitar dedupe.
    EmitsDuplicate { text: String },
    /// Falha a inferência do chunk.
    Fails { message: String },
    /// Aceita o áudio e não emite nada.
    Silent,
}

/// Registro do que o provider observou, para asserção nos testes.
#[derive(Debug, Default)]
pub struct FakeProviderLog {
    pub started_sessions: Mutex<Vec<(AudioSource, u64)>>,
    pub pushed_chunks: AtomicUsize,
    pub finished: AtomicUsize,
    pub cancelled: AtomicUsize,
}

impl FakeProviderLog {
    pub fn pushed(&self) -> usize {
        self.pushed_chunks.load(Ordering::SeqCst)
    }

    pub fn finish_count(&self) -> usize {
        self.finished.load(Ordering::SeqCst)
    }

    pub fn cancel_count(&self) -> usize {
        self.cancelled.load(Ordering::SeqCst)
    }

    pub fn sessions(&self) -> Vec<(AudioSource, u64)> {
        self.started_sessions.lock().unwrap().clone()
    }
}

pub struct FakeTranscriptionProvider {
    provider_id: TranscriptionProviderId,
    behavior: FakeBehavior,
    capabilities: TranscriptionCapabilities,
    pub log: Arc<FakeProviderLog>,
    fail_start: bool,
    failing_source: Option<AudioSource>,
}

impl FakeTranscriptionProvider {
    pub fn new(behavior: FakeBehavior) -> Self {
        FakeTranscriptionProvider {
            provider_id: TranscriptionProviderId::Fake,
            behavior,
            capabilities: TranscriptionCapabilities {
                local: true,
                streaming: true,
                partial_results: true,
                speaker_source_preserved: true,
                language_selection: true,
                automatic_language_detection: true,
                requires_credentials: false,
            },
            log: Arc::new(FakeProviderLog::default()),
            fail_start: false,
            failing_source: None,
        }
    }

    pub fn with_capabilities(mut self, capabilities: TranscriptionCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    pub fn with_provider_id(mut self, provider_id: TranscriptionProviderId) -> Self {
        self.provider_id = provider_id;
        self
    }

    /// Faz `start_session` falhar, para verificar que o runtime não engole a falha nem
    /// substitui o provider por outro.
    pub fn failing_to_start(mut self) -> Self {
        self.fail_start = true;
        self
    }

    /// Falha a inferência **apenas** dos chunks desta fonte. As duas fontes compartilham
    /// provider e runtime, e é justamente por isso que a falha de uma não pode derrubar a
    /// outra: quem fala na reunião é a outra pessoa (saída do sistema), e perder isso em
    /// silêncio por causa de um microfone com problema é o pior desfecho possível.
    pub fn failing_only_for(mut self, source: AudioSource) -> Self {
        self.failing_source = Some(source);
        self
    }

    pub fn log(&self) -> Arc<FakeProviderLog> {
        Arc::clone(&self.log)
    }
}

#[async_trait]
impl TranscriptionProvider for FakeTranscriptionProvider {
    fn id(&self) -> TranscriptionProviderId {
        self.provider_id
    }

    fn capabilities(&self) -> TranscriptionCapabilities {
        self.capabilities
    }

    async fn start_session(
        &self,
        context: TranscriptionSessionContext,
    ) -> Result<Box<dyn TranscriptionSession>, TranscriptionError> {
        if self.fail_start {
            return Err(TranscriptionError::ProviderUnavailable(
                "fake provider configured to fail".into(),
            ));
        }
        self.log
            .started_sessions
            .lock()
            .unwrap()
            .push((context.source, context.transcription_session_id.0));
        let behavior = match self.failing_source {
            Some(failing) if failing == context.source => FakeBehavior::Fails {
                message: format!("fake provider configured to fail for {failing:?}"),
            },
            _ => self.behavior.clone(),
        };
        Ok(Box::new(FakeSession {
            provider_id: self.provider_id,
            behavior,
            context,
            log: Arc::clone(&self.log),
            closed: false,
            sequence: 0,
        }))
    }
}

struct FakeSession {
    provider_id: TranscriptionProviderId,
    behavior: FakeBehavior,
    context: TranscriptionSessionContext,
    log: Arc<FakeProviderLog>,
    closed: bool,
    sequence: u64,
}

impl FakeSession {
    fn payload(
        &self,
        text: String,
        is_final: bool,
        event_id: ProviderEventId,
    ) -> TranscriptPayload {
        TranscriptPayload {
            session_id: self.context.session_id,
            transcription_session_id: self.context.transcription_session_id,
            source: self.context.source,
            provider: self.provider_id,
            language: Some("pt".into()),
            text,
            started_at: crate::audio::segment::AudioTimestamp(0),
            ended_at: crate::audio::segment::AudioTimestamp(1_000),
            confidence: Some(0.9),
            is_final,
            provider_event_id: event_id,
            segment_id: None,
            processing_time_ms: Some(1),
        }
    }

    fn event_id(&self, suffix: &str) -> ProviderEventId {
        ProviderEventId::new(format!(
            "fake:{}:{}:{suffix}",
            self.context.transcription_session_id, self.sequence
        ))
    }
}

#[async_trait]
impl TranscriptionSession for FakeSession {
    async fn push_audio(&mut self, chunk: AudioChunk) -> Result<(), TranscriptionError> {
        if self.closed {
            return Err(TranscriptionError::SessionClosed);
        }
        if chunk.source != self.context.source {
            return Err(TranscriptionError::SourceMismatch {
                expected: self.context.source,
                received: chunk.source,
            });
        }
        self.log.pushed_chunks.fetch_add(1, Ordering::SeqCst);
        self.sequence += 1;

        self.context
            .emit(TranscriptionEvent::SpeechStarted(SpeechBoundary {
                session_id: self.context.session_id,
                transcription_session_id: self.context.transcription_session_id,
                source: self.context.source,
                provider: self.provider_id,
                at: chunk.started_at,
                provider_event_id: self.event_id("speech-started"),
            }));

        match self.behavior.clone() {
            FakeBehavior::EmitsFinal { text, partials } => {
                if partials {
                    let id = self.event_id("partial");
                    let payload = self.payload(text.clone(), false, id);
                    self.context
                        .emit(TranscriptionEvent::Partial(PartialTranscript(payload)));
                }
                let id = self.event_id("final");
                let payload = self.payload(text, true, id);
                self.context
                    .emit(TranscriptionEvent::Final(FinalTranscript(payload)));
            }
            FakeBehavior::EmitsFinalAfter { text, delay } => {
                tokio::time::sleep(delay).await;
                let id = self.event_id("final");
                let payload = self.payload(text, true, id);
                self.context
                    .emit(TranscriptionEvent::Final(FinalTranscript(payload)));
            }
            FakeBehavior::EmitsDuplicate { text } => {
                let id = self.event_id("final");
                for _ in 0..2 {
                    let payload = self.payload(text.clone(), true, id.clone());
                    self.context
                        .emit(TranscriptionEvent::Final(FinalTranscript(payload)));
                }
            }
            FakeBehavior::Fails { message } => {
                self.context
                    .emit(TranscriptionEvent::Error(TranscriptionErrorEvent {
                        session_id: self.context.session_id,
                        transcription_session_id: self.context.transcription_session_id,
                        source: self.context.source,
                        provider: self.provider_id,
                        message: message.clone(),
                        recoverable: true,
                    }));
                return Err(TranscriptionError::InferenceFailed(message));
            }
            FakeBehavior::Silent => {}
        }

        self.context
            .emit(TranscriptionEvent::SpeechEnded(SpeechBoundary {
                session_id: self.context.session_id,
                transcription_session_id: self.context.transcription_session_id,
                source: self.context.source,
                provider: self.provider_id,
                at: chunk.ended_at,
                provider_event_id: self.event_id("speech-ended"),
            }));
        Ok(())
    }

    async fn finish(&mut self) -> Result<(), TranscriptionError> {
        self.closed = true;
        self.log.finished.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn cancel(&mut self) -> Result<(), TranscriptionError> {
        self.closed = true;
        self.log.cancelled.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
