//! Gemini Live transcription provider using Google's official WebSocket protocol.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::audio::segment::AudioTimestamp;
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
use crate::transcription::settings::GEMINI_LIVE_ENDPOINT;

const GEMINI_INPUT_SAMPLE_RATE: u32 = 16_000;
const SETUP_TIMEOUT: Duration = Duration::from_secs(10);
const FINAL_TRANSCRIPT_GRACE: Duration = Duration::from_millis(800);

type GeminiSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type GeminiWriter = SplitSink<GeminiSocket, Message>;
type GeminiReader = SplitStream<GeminiSocket>;
type ApiKeyLoader = Arc<dyn Fn() -> Result<Option<String>, TranscriptionError> + Send + Sync>;

#[derive(Clone)]
pub struct GeminiLiveTranscriptionProvider {
    api_key_loader: ApiKeyLoader,
}

impl Default for GeminiLiveTranscriptionProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl GeminiLiveTranscriptionProvider {
    pub fn new() -> Self {
        Self {
            api_key_loader: Arc::new(|| {
                crate::transcription::secrets::load_api_key(TranscriptionProviderId::GoogleGemini)
                    .map_err(|error| TranscriptionError::ProviderUnavailable(error.to_string()))
            }),
        }
    }

    #[cfg(test)]
    fn with_api_key_loader(api_key_loader: ApiKeyLoader) -> Self {
        Self { api_key_loader }
    }

    fn api_key(&self) -> Result<String, TranscriptionError> {
        (self.api_key_loader)()?
            .filter(|key| !key.trim().is_empty())
            .ok_or_else(|| {
                TranscriptionError::MissingCredentials(
                    "Gemini Live API key is not configured in the system keychain".into(),
                )
            })
    }
}

#[async_trait]
impl TranscriptionProvider for GeminiLiveTranscriptionProvider {
    fn id(&self) -> TranscriptionProviderId {
        TranscriptionProviderId::GoogleGemini
    }

    fn capabilities(&self) -> TranscriptionCapabilities {
        TranscriptionCapabilities {
            local: false,
            streaming: true,
            partial_results: true,
            speaker_source_preserved: true,
            language_selection: false,
            automatic_language_detection: true,
            requires_credentials: true,
        }
    }

    async fn start_session(
        &self,
        context: TranscriptionSessionContext,
    ) -> Result<Box<dyn TranscriptionSession>, TranscriptionError> {
        let api_key = self.api_key()?;
        let model = context
            .model
            .clone()
            .ok_or(TranscriptionError::NotConfigured)?;
        if model.trim().is_empty() {
            return Err(TranscriptionError::NotConfigured);
        }
        let qualified_model = if model.trim().starts_with("models/") {
            model.trim().to_string()
        } else {
            format!("models/{}", model.trim())
        };

        let mut endpoint = Url::parse(GEMINI_LIVE_ENDPOINT).map_err(|error| {
            TranscriptionError::ProviderUnavailable(format!(
                "invalid official Gemini Live endpoint: {error}"
            ))
        })?;
        endpoint
            .query_pairs_mut()
            .append_pair("key", api_key.trim());

        let (mut socket, _) = tokio_tungstenite::connect_async(endpoint.as_str())
            .await
            .map_err(connection_error)?;
        socket
            .send(Message::Text(
                json!({
                    "setup": {
                        "model": qualified_model,
                        "generationConfig": { "responseModalities": ["AUDIO"] },
                        "inputAudioTranscription": {}
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .map_err(connection_error)?;

        wait_for_setup_complete(&mut socket).await?;
        let (writer, reader) = socket.split();
        Ok(Box::new(GeminiLiveSession::new(context, writer, reader)))
    }

    async fn readiness(&self) -> Result<(), TranscriptionError> {
        self.api_key().map(|_| ())
    }
}

async fn wait_for_setup_complete(socket: &mut GeminiSocket) -> Result<(), TranscriptionError> {
    tokio::time::timeout(SETUP_TIMEOUT, async {
        while let Some(message) = socket.next().await {
            match message.map_err(connection_error)? {
                Message::Text(text) => {
                    let payload: Value = serde_json::from_str(text.as_ref()).map_err(|error| {
                        TranscriptionError::ProviderUnavailable(format!(
                            "invalid Gemini Live setup response: {error}"
                        ))
                    })?;
                    if payload.get("setupComplete").is_some() {
                        return Ok(());
                    }
                    if let Some(error) = payload.get("error") {
                        return Err(TranscriptionError::ProviderUnavailable(format!(
                            "Gemini Live setup failed: {error}"
                        )));
                    }
                }
                Message::Close(frame) => {
                    return Err(TranscriptionError::ProviderUnavailable(format!(
                        "Gemini Live closed during setup: {frame:?}"
                    )));
                }
                _ => {}
            }
        }
        Err(TranscriptionError::ProviderUnavailable(
            "Gemini Live closed before setup completed".into(),
        ))
    })
    .await
    .map_err(|_| TranscriptionError::ProviderUnavailable("Gemini Live setup timed out".into()))?
}

fn connection_error(error: impl std::fmt::Display) -> TranscriptionError {
    TranscriptionError::ProviderUnavailable(format!("Gemini Live connection failed: {error}"))
}

#[derive(Default)]
struct AudioTiming {
    started_at: Option<AudioTimestamp>,
    ended_at: Option<AudioTimestamp>,
}

struct GeminiLiveSession {
    context: TranscriptionSessionContext,
    writer: GeminiWriter,
    cancellation: CancellationToken,
    receiver: Option<JoinHandle<()>>,
    closing: Arc<AtomicBool>,
    graceful: Arc<AtomicBool>,
    timing: Arc<Mutex<AudioTiming>>,
    final_sequence: Arc<AtomicU64>,
    final_notification: Arc<tokio::sync::Notify>,
    closed: bool,
}

impl GeminiLiveSession {
    fn new(
        context: TranscriptionSessionContext,
        writer: GeminiWriter,
        reader: GeminiReader,
    ) -> Self {
        let cancellation = CancellationToken::new();
        let closing = Arc::new(AtomicBool::new(false));
        let graceful = Arc::new(AtomicBool::new(false));
        let timing = Arc::new(Mutex::new(AudioTiming::default()));
        let final_sequence = Arc::new(AtomicU64::new(0));
        let final_notification = Arc::new(tokio::sync::Notify::new());
        let receiver = tokio::spawn(receive_messages(ReceiverTask {
            context: context.clone(),
            reader,
            cancellation: cancellation.clone(),
            closing: Arc::clone(&closing),
            graceful: Arc::clone(&graceful),
            timing: Arc::clone(&timing),
            sequence: Arc::clone(&final_sequence),
            final_notification: Arc::clone(&final_notification),
        }));

        Self {
            context,
            writer,
            cancellation,
            receiver: Some(receiver),
            closing,
            graceful,
            timing,
            final_sequence,
            final_notification,
            closed: false,
        }
    }

    fn ensure_open(&self) -> Result<(), TranscriptionError> {
        if self.closed {
            Err(TranscriptionError::SessionClosed)
        } else {
            Ok(())
        }
    }

    async fn stop_receiver(&mut self) {
        self.cancellation.cancel();
        if let Some(receiver) = self.receiver.take() {
            let _ = receiver.await;
        }
    }
}

#[async_trait]
impl TranscriptionSession for GeminiLiveSession {
    async fn push_audio(&mut self, chunk: AudioChunk) -> Result<(), TranscriptionError> {
        self.ensure_open()?;
        if chunk.source != self.context.source {
            return Err(TranscriptionError::SourceMismatch {
                expected: self.context.source,
                received: chunk.source,
            });
        }
        if chunk.sample_rate != GEMINI_INPUT_SAMPLE_RATE {
            return Err(TranscriptionError::ProviderUnavailable(format!(
                "Gemini Live requires {GEMINI_INPUT_SAMPLE_RATE}Hz PCM, received {}Hz",
                chunk.sample_rate
            )));
        }

        {
            let mut timing = self
                .timing
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            timing.started_at.get_or_insert(chunk.started_at);
            timing.ended_at = Some(chunk.ended_at);
        }

        let bytes = pcm_i16_le(&chunk.samples);
        self.writer
            .send(Message::Text(
                json!({
                    "realtimeInput": {
                        "audio": {
                            "data": BASE64.encode(bytes),
                            "mimeType": "audio/pcm;rate=16000"
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .map_err(connection_error)
    }

    async fn finish(&mut self) -> Result<(), TranscriptionError> {
        self.ensure_open()?;
        self.graceful.store(true, Ordering::Release);
        let before = self.final_sequence.load(Ordering::Acquire);
        self.writer
            .send(Message::Text(
                json!({ "realtimeInput": { "audioStreamEnd": true } })
                    .to_string()
                    .into(),
            ))
            .await
            .map_err(connection_error)?;

        if self.final_sequence.load(Ordering::Acquire) == before {
            let _ =
                tokio::time::timeout(FINAL_TRANSCRIPT_GRACE, self.final_notification.notified())
                    .await;
        }

        self.closing.store(true, Ordering::Release);
        let close_result = self.writer.send(Message::Close(None)).await;
        self.closed = true;
        self.stop_receiver().await;
        close_result.map_err(connection_error)
    }

    async fn cancel(&mut self) -> Result<(), TranscriptionError> {
        if self.closed {
            return Ok(());
        }
        self.closing.store(true, Ordering::Release);
        self.graceful.store(false, Ordering::Release);
        let close_result = self.writer.send(Message::Close(None)).await;
        self.closed = true;
        self.stop_receiver().await;
        close_result.map_err(connection_error)
    }
}

fn pcm_i16_le(samples: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        let value = (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

struct ReceiverTask {
    context: TranscriptionSessionContext,
    reader: GeminiReader,
    cancellation: CancellationToken,
    closing: Arc<AtomicBool>,
    graceful: Arc<AtomicBool>,
    timing: Arc<Mutex<AudioTiming>>,
    sequence: Arc<AtomicU64>,
    final_notification: Arc<tokio::sync::Notify>,
}

async fn receive_messages(task: ReceiverTask) {
    let ReceiverTask {
        context,
        mut reader,
        cancellation,
        closing,
        graceful,
        timing,
        sequence,
        final_notification,
    } = task;
    let mut transcript = String::new();
    let mut speech_started = false;

    loop {
        tokio::select! {
            _ = cancellation.cancelled() => {
                if graceful.load(Ordering::Acquire) {
                    emit_final(&context, &mut transcript, &mut speech_started, &timing, &sequence);
                }
                break;
            }
            message = reader.next() => {
                let Some(message) = message else {
                    if !closing.load(Ordering::Acquire) {
                        emit_error(&context, "Gemini Live connection closed unexpectedly".into());
                    }
                    break;
                };
                match message {
                    Ok(Message::Text(text)) => {
                        if let Err(error) = handle_server_message(
                            &context,
                            text.as_ref(),
                            &mut transcript,
                            &mut speech_started,
                            &timing,
                            &sequence,
                            &final_notification,
                        ) {
                            emit_error(&context, error);
                        }
                    }
                    Ok(Message::Close(_)) => {
                        if !closing.load(Ordering::Acquire) {
                            emit_error(&context, "Gemini Live connection closed unexpectedly".into());
                        }
                        break;
                    }
                    Err(error) => {
                        if !closing.load(Ordering::Acquire) {
                            emit_error(&context, format!("Gemini Live receive failed: {error}"));
                        }
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
}

fn handle_server_message(
    context: &TranscriptionSessionContext,
    text: &str,
    transcript: &mut String,
    speech_started: &mut bool,
    timing: &Arc<Mutex<AudioTiming>>,
    sequence: &AtomicU64,
    final_notification: &tokio::sync::Notify,
) -> Result<(), String> {
    let payload: Value = serde_json::from_str(text)
        .map_err(|error| format!("invalid Gemini Live response: {error}"))?;
    if let Some(error) = payload.get("error") {
        return Err(format!("Gemini Live returned an error: {error}"));
    }

    let Some(server_content) = payload.get("serverContent") else {
        return Ok(());
    };

    if let Some(fragment) = server_content
        .get("inputTranscription")
        .and_then(|value| value.get("text"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        transcript.push_str(fragment);
        if !*speech_started {
            *speech_started = true;
            let (started_at, _) = current_timing(timing);
            context.emit(TranscriptionEvent::SpeechStarted(SpeechBoundary {
                session_id: context.session_id,
                transcription_session_id: context.transcription_session_id,
                source: context.source,
                provider: TranscriptionProviderId::GoogleGemini,
                at: started_at,
                provider_event_id: next_event_id(context, sequence, "speech-started"),
            }));
        }
        let (started_at, ended_at) = current_timing(timing);
        context.emit(TranscriptionEvent::Partial(PartialTranscript(
            TranscriptPayload {
                session_id: context.session_id,
                transcription_session_id: context.transcription_session_id,
                source: context.source,
                provider: TranscriptionProviderId::GoogleGemini,
                language: None,
                text: transcript.clone(),
                started_at,
                ended_at,
                confidence: None,
                is_final: false,
                provider_event_id: next_event_id(context, sequence, "partial"),
                segment_id: None,
                processing_time_ms: None,
            },
        )));
    }

    if server_content
        .get("turnComplete")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        emit_final(context, transcript, speech_started, timing, sequence);
        final_notification.notify_waiters();
    }

    Ok(())
}

fn emit_final(
    context: &TranscriptionSessionContext,
    transcript: &mut String,
    speech_started: &mut bool,
    timing: &Arc<Mutex<AudioTiming>>,
    sequence: &AtomicU64,
) {
    let text = std::mem::take(transcript);
    if text.trim().is_empty() {
        return;
    }
    let (started_at, ended_at) = take_timing(timing);
    if *speech_started {
        context.emit(TranscriptionEvent::SpeechEnded(SpeechBoundary {
            session_id: context.session_id,
            transcription_session_id: context.transcription_session_id,
            source: context.source,
            provider: TranscriptionProviderId::GoogleGemini,
            at: ended_at,
            provider_event_id: next_event_id(context, sequence, "speech-ended"),
        }));
    }
    *speech_started = false;
    context.emit(TranscriptionEvent::Final(FinalTranscript(
        TranscriptPayload {
            session_id: context.session_id,
            transcription_session_id: context.transcription_session_id,
            source: context.source,
            provider: TranscriptionProviderId::GoogleGemini,
            language: None,
            text,
            started_at,
            ended_at,
            confidence: None,
            is_final: true,
            provider_event_id: next_event_id(context, sequence, "final"),
            segment_id: None,
            processing_time_ms: None,
        },
    )));
}

fn current_timing(timing: &Arc<Mutex<AudioTiming>>) -> (AudioTimestamp, AudioTimestamp) {
    let timing = timing
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    (
        timing.started_at.unwrap_or(AudioTimestamp(0)),
        timing.ended_at.unwrap_or(AudioTimestamp(0)),
    )
}

fn take_timing(timing: &Arc<Mutex<AudioTiming>>) -> (AudioTimestamp, AudioTimestamp) {
    let mut timing = timing
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let result = (
        timing.started_at.unwrap_or(AudioTimestamp(0)),
        timing.ended_at.unwrap_or(AudioTimestamp(0)),
    );
    timing.started_at = None;
    timing.ended_at = None;
    result
}

fn next_event_id(
    context: &TranscriptionSessionContext,
    sequence: &AtomicU64,
    kind: &str,
) -> ProviderEventId {
    ProviderEventId::new(format!(
        "{}:gemini-live:{kind}:{}",
        context.transcription_session_id.0,
        sequence.fetch_add(1, Ordering::Relaxed)
    ))
}

fn emit_error(context: &TranscriptionSessionContext, message: String) {
    context.emit(TranscriptionEvent::Error(TranscriptionErrorEvent {
        session_id: context.session_id,
        transcription_session_id: context.transcription_session_id,
        source: context.source,
        provider: TranscriptionProviderId::GoogleGemini,
        message,
        recoverable: false,
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::types::AudioSource;
    use crate::conversation::SessionId;
    use crate::transcription::session::TranscriptionSessionId;
    use crate::transcription::types::TranscriptionLanguage;

    #[test]
    fn pcm_conversion_is_little_endian_and_clamped() {
        assert_eq!(pcm_i16_le(&[-2.0, 0.0, 2.0]), vec![1, 128, 0, 0, 255, 127]);
    }

    #[tokio::test]
    async fn readiness_uses_injected_key_source() {
        let configured = GeminiLiveTranscriptionProvider::with_api_key_loader(Arc::new(|| {
            Ok(Some("test-key".into()))
        }));
        assert!(configured.readiness().await.is_ok());

        let missing = GeminiLiveTranscriptionProvider::with_api_key_loader(Arc::new(|| Ok(None)));
        assert!(matches!(
            missing.readiness().await,
            Err(TranscriptionError::MissingCredentials(_))
        ));
    }

    #[test]
    fn input_transcription_becomes_partial_then_final_with_stable_identity() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&events);
        let context = TranscriptionSessionContext {
            session_id: SessionId::from_value(42),
            transcription_session_id: TranscriptionSessionId::next(),
            source: AudioSource::SystemOutput,
            language: TranscriptionLanguage::Automatic,
            model: Some("test-model".into()),
            sink: Arc::new(move |event| captured.lock().unwrap().push(event)),
        };
        let timing = Arc::new(Mutex::new(AudioTiming {
            started_at: Some(AudioTimestamp(100)),
            ended_at: Some(AudioTimestamp(500)),
        }));
        let sequence = AtomicU64::new(0);
        let notification = tokio::sync::Notify::new();
        let mut transcript = String::new();
        let mut speech_started = false;

        handle_server_message(
            &context,
            r#"{"serverContent":{"inputTranscription":{"text":"olá"}}}"#,
            &mut transcript,
            &mut speech_started,
            &timing,
            &sequence,
            &notification,
        )
        .unwrap();
        handle_server_message(
            &context,
            r#"{"serverContent":{"turnComplete":true}}"#,
            &mut transcript,
            &mut speech_started,
            &timing,
            &sequence,
            &notification,
        )
        .unwrap();

        let events = events.lock().unwrap();
        assert_eq!(events.len(), 4);
        let TranscriptionEvent::Partial(partial) = &events[1] else {
            panic!("second event must be a partial transcript");
        };
        let TranscriptionEvent::Final(final_transcript) = &events[3] else {
            panic!("fourth event must be a final transcript");
        };
        assert_eq!(partial.text, "olá");
        assert_eq!(final_transcript.text, "olá");
        assert_eq!(partial.session_id, SessionId::from_value(42));
        assert_eq!(
            partial.transcription_session_id,
            context.transcription_session_id
        );
        assert_eq!(partial.source, AudioSource::SystemOutput);
        assert_eq!(partial.provider, TranscriptionProviderId::GoogleGemini);
        assert_ne!(
            partial.provider_event_id,
            final_transcript.provider_event_id
        );
        assert!(!partial.is_final);
        assert!(final_transcript.is_final);
    }
}
