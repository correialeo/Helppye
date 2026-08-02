//! Gemini Live transcription provider using Google's official WebSocket protocol.
//!
//! Input transcription has no explicit final flag. Helppye therefore owns the input
//! activity lifecycle with its local VAD and finalizes after a short post-activity drain;
//! model `turnComplete` is diagnostic only and never controls transcript finalization.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant as StdInstant};

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use url::Url;

use crate::audio::segment::AudioTimestamp;
use crate::telemetry::ProviderTelemetryEvent;
use crate::transcription::error::TranscriptionError;
use crate::transcription::events::{
    FinalTranscript, PartialTranscript, ProviderEventId, SpeechBoundary, TranscriptPayload,
    TranscriptionErrorEvent, TranscriptionEvent,
};
use crate::transcription::provider::{
    TranscriptionCapabilities, TranscriptionProvider, TranscriptionProviderId,
};
use crate::transcription::session::{
    AudioActivity, AudioChunk, StreamingAudioConfig, TranscriptionSession,
    TranscriptionSessionContext,
};
use crate::transcription::settings::{
    DEFAULT_GEMINI_AUDIO_CHUNK_MS, DEFAULT_GEMINI_FINALIZATION_TIMEOUT_MS,
    DEFAULT_GEMINI_TRANSCRIPT_DRAIN_MS, DEFAULT_MANUAL_ACTIVITY_END_SILENCE_MS,
    GEMINI_LIVE_ENDPOINT,
};

const GEMINI_INPUT_SAMPLE_RATE: u32 = 16_000;
const SETUP_TIMEOUT: Duration = Duration::from_secs(10);

type GeminiSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type GeminiWriter = SplitSink<GeminiSocket, Message>;
type GeminiReader = SplitStream<GeminiSocket>;
type ApiKeyLoader = Arc<dyn Fn() -> Result<Option<String>, TranscriptionError> + Send + Sync>;

#[async_trait]
trait GeminiMessageSink: Send {
    async fn send_message(&mut self, message: Message) -> Result<(), TranscriptionError>;
}

#[async_trait]
impl GeminiMessageSink for GeminiWriter {
    async fn send_message(&mut self, message: Message) -> Result<(), TranscriptionError> {
        SinkExt::send(self, message).await.map_err(connection_error)
    }
}

#[async_trait]
trait GeminiMessageStream: Send {
    async fn next_message(&mut self) -> Option<Result<Message, String>>;
}

#[async_trait]
impl GeminiMessageStream for GeminiReader {
    async fn next_message(&mut self) -> Option<Result<Message, String>> {
        StreamExt::next(self)
            .await
            .map(|message| message.map_err(|error| error.to_string()))
    }
}

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
        Self::with_api_key_loader(Arc::new(|| {
            crate::transcription::secrets::load_api_key(TranscriptionProviderId::GoogleGemini)
                .map_err(|error| TranscriptionError::ProviderUnavailable(error.to_string()))
        }))
    }

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
            language_selection: true,
            automatic_language_detection: true,
            requires_credentials: true,
        }
    }

    fn streaming_audio_config(
        &self,
        settings: &crate::transcription::settings::TranscriptionSettings,
    ) -> Option<StreamingAudioConfig> {
        let gemini = &settings.providers.google_gemini;
        Some(StreamingAudioConfig {
            manual_activity_detection: true,
            activity_end_silence_ms: gemini.manual_activity_end_silence_ms,
            target_chunk_ms: gemini.audio_chunk_ms,
            transcript_drain_ms: gemini.transcript_drain_ms,
            finalization_timeout_ms: gemini.finalization_timeout_ms,
        })
    }

    async fn start_session(
        &self,
        context: TranscriptionSessionContext,
    ) -> Result<Box<dyn TranscriptionSession>, TranscriptionError> {
        let api_key = self.api_key()?;
        let model = context
            .model
            .as_deref()
            .filter(|model| !model.trim().is_empty())
            .ok_or(TranscriptionError::NotConfigured)?;
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

        let connect_started = StdInstant::now();
        let (mut socket, _) = tokio_tungstenite::connect_async(endpoint.as_str())
            .await
            .map_err(connection_error)?;
        let websocket_connect_ms = elapsed_ms(connect_started);
        socket
            .send(Message::Text(
                json!({
                    "setup": {
                        "model": qualified_model,
                        "generationConfig": { "responseModalities": ["AUDIO"] },
                        "inputAudioTranscription": {},
                        "realtimeInputConfig": {
                            "automaticActivityDetection": { "disabled": true }
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .map_err(connection_error)?;
        let setup_started = StdInstant::now();
        wait_for_setup_complete(&mut socket).await?;
        info!(
            websocket_connect_ms,
            setup_complete_ms = elapsed_ms(setup_started),
            automatic_vad_enabled = false,
            "Gemini Live session ready before audio capture"
        );

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
                message @ (Message::Text(_) | Message::Binary(_)) => {
                    let payload = parse_json_message(&message).map_err(|error| {
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
                    return Err(TranscriptionError::ProviderUnavailable(format!(
                        "unexpected Gemini Live setup response: {payload}"
                    )));
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

fn parse_json_message(message: &Message) -> Result<Value, serde_json::Error> {
    match message {
        Message::Text(text) => serde_json::from_str(text.as_ref()),
        Message::Binary(bytes) => serde_json::from_slice(bytes.as_ref()),
        _ => unreachable!("caller only passes text or binary JSON"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FinalizationReason {
    TranscriptStabilized,
    ProviderDrainTimeout,
    SupersededByNextActivity,
}

impl FinalizationReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::TranscriptStabilized => "transcript_stabilized",
            Self::ProviderDrainTimeout => "provider_drain_timeout",
            Self::SupersededByNextActivity => "superseded_by_next_activity",
        }
    }
}

#[derive(Debug)]
enum GeminiInputTurnState {
    Idle,
    Active {
        activity_id: u64,
        accumulated_text: String,
        latest_revision: u64,
    },
    Draining {
        activity_id: u64,
        accumulated_text: String,
        activity_end_at: Instant,
        latest_revision: u64,
    },
}

#[derive(Debug)]
struct InputTurnMachine {
    state: GeminiInputTurnState,
    started_at: AudioTimestamp,
    ended_at: AudioTimestamp,
    last_revision_at: Option<Instant>,
    drain: Duration,
    absolute_timeout: Duration,
}

#[derive(Debug)]
struct FinalizedInput {
    activity_id: u64,
    text: String,
    started_at: AudioTimestamp,
    ended_at: AudioTimestamp,
    revision_count: u64,
    activity_end_at: Instant,
    last_revision_at: Option<Instant>,
    reason: FinalizationReason,
}

impl InputTurnMachine {
    fn new(config: StreamingAudioConfig) -> Self {
        Self {
            state: GeminiInputTurnState::Idle,
            started_at: AudioTimestamp(0),
            ended_at: AudioTimestamp(0),
            last_revision_at: None,
            drain: Duration::from_millis(u64::from(config.transcript_drain_ms)),
            absolute_timeout: Duration::from_millis(u64::from(config.finalization_timeout_ms)),
        }
    }

    fn begin(&mut self, activity_id: u64, at: AudioTimestamp) -> Option<FinalizedInput> {
        let previous = self.take_now(FinalizationReason::SupersededByNextActivity);
        self.started_at = at;
        self.ended_at = at;
        self.last_revision_at = None;
        self.state = GeminiInputTurnState::Active {
            activity_id,
            accumulated_text: String::new(),
            latest_revision: 0,
        };
        previous
    }

    fn update_audio_end(&mut self, at: AudioTimestamp) {
        self.ended_at = at;
    }

    fn end(&mut self, now: Instant, at: AudioTimestamp) {
        self.ended_at = at;
        let state = std::mem::replace(&mut self.state, GeminiInputTurnState::Idle);
        self.state = match state {
            GeminiInputTurnState::Active {
                activity_id,
                accumulated_text,
                latest_revision,
            }
            | GeminiInputTurnState::Draining {
                activity_id,
                accumulated_text,
                latest_revision,
                ..
            } => GeminiInputTurnState::Draining {
                activity_id,
                accumulated_text,
                activity_end_at: now,
                latest_revision,
            },
            GeminiInputTurnState::Idle => GeminiInputTurnState::Idle,
        };
    }

    fn update_transcript(&mut self, fragment: &str, now: Instant) -> Option<(String, u64)> {
        let (text, revision) = match &mut self.state {
            GeminiInputTurnState::Active {
                accumulated_text,
                latest_revision,
                ..
            }
            | GeminiInputTurnState::Draining {
                accumulated_text,
                latest_revision,
                ..
            } => (accumulated_text, latest_revision),
            GeminiInputTurnState::Idle => return None,
        };
        let merged = merge_input_transcription(text, fragment);
        if merged == *text {
            return None;
        }
        *text = merged;
        *revision += 1;
        self.last_revision_at = Some(now);
        Some((text.clone(), *revision))
    }

    fn next_deadline(&self) -> Option<Instant> {
        let GeminiInputTurnState::Draining {
            accumulated_text,
            activity_end_at,
            ..
        } = &self.state
        else {
            return None;
        };
        let absolute = *activity_end_at + self.absolute_timeout;
        if accumulated_text.trim().is_empty() {
            return Some(absolute);
        }
        let stable = self.last_revision_at.unwrap_or(*activity_end_at) + self.drain;
        Some(stable.min(absolute))
    }

    fn take_if_due(&mut self, now: Instant) -> Option<FinalizedInput> {
        let GeminiInputTurnState::Draining {
            activity_end_at,
            accumulated_text,
            ..
        } = &self.state
        else {
            return None;
        };
        let absolute_due = now >= *activity_end_at + self.absolute_timeout;
        let stable_due = !accumulated_text.trim().is_empty()
            && now >= self.last_revision_at.unwrap_or(*activity_end_at) + self.drain;
        if !absolute_due && !stable_due {
            return None;
        }
        let reason = if absolute_due {
            FinalizationReason::ProviderDrainTimeout
        } else {
            FinalizationReason::TranscriptStabilized
        };
        self.take_now(reason)
    }

    fn take_now(&mut self, reason: FinalizationReason) -> Option<FinalizedInput> {
        let state = std::mem::replace(&mut self.state, GeminiInputTurnState::Idle);
        let GeminiInputTurnState::Draining {
            activity_id,
            accumulated_text,
            activity_end_at,
            latest_revision,
        } = state
        else {
            self.state = state;
            return None;
        };
        Some(FinalizedInput {
            activity_id,
            text: accumulated_text,
            started_at: self.started_at,
            ended_at: self.ended_at,
            revision_count: latest_revision,
            activity_end_at,
            last_revision_at: self.last_revision_at,
            reason,
        })
    }

    fn is_active(&self) -> bool {
        matches!(self.state, GeminiInputTurnState::Active { .. })
    }

    fn is_idle(&self) -> bool {
        matches!(self.state, GeminiInputTurnState::Idle)
    }

    fn current_timing(&self) -> (AudioTimestamp, AudioTimestamp) {
        (self.started_at, self.ended_at)
    }
}

fn merge_input_transcription(current: &str, incoming: &str) -> String {
    if incoming.is_empty() || incoming == current {
        return current.to_string();
    }
    if current.is_empty() || incoming.starts_with(current) {
        return incoming.to_string();
    }
    if current.starts_with(incoming) {
        return current.to_string();
    }

    let common_prefix = current
        .chars()
        .zip(incoming.chars())
        .take_while(|(left, right)| left == right)
        .count();
    if common_prefix >= 4 && incoming.chars().any(char::is_whitespace) {
        return incoming.to_string();
    }

    let max_overlap = current.len().min(incoming.len());
    for overlap in (1..=max_overlap).rev() {
        if current.is_char_boundary(current.len() - overlap)
            && incoming.is_char_boundary(overlap)
            && current[current.len() - overlap..] == incoming[..overlap]
        {
            return format!("{}{}", current, &incoming[overlap..]);
        }
    }

    let needs_space = !current.ends_with(char::is_whitespace)
        && !incoming.starts_with(char::is_whitespace)
        && !incoming.starts_with(|character: char| ",.!?;:)".contains(character));
    if needs_space {
        format!("{current} {incoming}")
    } else {
        format!("{current}{incoming}")
    }
}

#[derive(Debug)]
struct PcmChunker {
    target_samples: usize,
    pending: Vec<f32>,
}

impl PcmChunker {
    fn new(target_samples: usize) -> Self {
        Self {
            target_samples: target_samples.max(1),
            pending: Vec::new(),
        }
    }

    fn push(&mut self, samples: &[f32]) -> Vec<Vec<f32>> {
        self.pending.extend_from_slice(samples);
        let mut chunks = Vec::new();
        while self.pending.len() >= self.target_samples {
            chunks.push(self.pending.drain(..self.target_samples).collect());
        }
        chunks
    }

    fn flush(&mut self) -> Option<Vec<f32>> {
        if self.pending.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.pending))
        }
    }
}

struct GeminiLiveSession {
    context: TranscriptionSessionContext,
    writer: Box<dyn GeminiMessageSink>,
    receiver: Option<JoinHandle<()>>,
    cancellation: CancellationToken,
    closed: Arc<AtomicBool>,
    closing: Arc<AtomicBool>,
    state: Arc<Mutex<InputTurnMachine>>,
    state_changed: Arc<tokio::sync::Notify>,
    final_notification: Arc<tokio::sync::Notify>,
    sequence: Arc<AtomicU64>,
    next_activity_id: u64,
    chunker: PcmChunker,
    audio_chunks_sent: u64,
    bytes_sent: u64,
    finalization_timeout: Duration,
}

impl GeminiLiveSession {
    fn new(
        context: TranscriptionSessionContext,
        writer: GeminiWriter,
        reader: GeminiReader,
    ) -> Self {
        Self::new_with_transport(context, Box::new(writer), Box::new(reader))
    }

    fn new_with_transport(
        context: TranscriptionSessionContext,
        writer: Box<dyn GeminiMessageSink>,
        reader: Box<dyn GeminiMessageStream>,
    ) -> Self {
        let config = context
            .streaming_audio_config
            .unwrap_or(StreamingAudioConfig {
                manual_activity_detection: true,
                activity_end_silence_ms: DEFAULT_MANUAL_ACTIVITY_END_SILENCE_MS,
                target_chunk_ms: DEFAULT_GEMINI_AUDIO_CHUNK_MS,
                transcript_drain_ms: DEFAULT_GEMINI_TRANSCRIPT_DRAIN_MS,
                finalization_timeout_ms: DEFAULT_GEMINI_FINALIZATION_TIMEOUT_MS,
            });
        let cancellation = CancellationToken::new();
        let closed = Arc::new(AtomicBool::new(false));
        let closing = Arc::new(AtomicBool::new(false));
        let state = Arc::new(Mutex::new(InputTurnMachine::new(config)));
        let state_changed = Arc::new(tokio::sync::Notify::new());
        let final_notification = Arc::new(tokio::sync::Notify::new());
        let sequence = Arc::new(AtomicU64::new(0));
        let receiver = tokio::spawn(receive_messages(ReceiverTask {
            context: context.clone(),
            reader,
            cancellation: cancellation.clone(),
            closing: Arc::clone(&closing),
            state: Arc::clone(&state),
            state_changed: Arc::clone(&state_changed),
            final_notification: Arc::clone(&final_notification),
            sequence: Arc::clone(&sequence),
        }));
        Self {
            context,
            writer,
            receiver: Some(receiver),
            cancellation,
            closed,
            closing,
            state,
            state_changed,
            final_notification,
            sequence,
            next_activity_id: 1,
            chunker: PcmChunker::new(
                (GEMINI_INPUT_SAMPLE_RATE as usize * config.target_chunk_ms as usize) / 1_000,
            ),
            audio_chunks_sent: 0,
            bytes_sent: 0,
            finalization_timeout: Duration::from_millis(u64::from(config.finalization_timeout_ms)),
        }
    }

    fn ensure_open(&self) -> Result<(), TranscriptionError> {
        if self.closed.load(Ordering::Acquire) {
            Err(TranscriptionError::SessionClosed)
        } else {
            Ok(())
        }
    }

    async fn send_value(&mut self, value: Value) -> Result<u64, TranscriptionError> {
        let started = StdInstant::now();
        self.writer
            .send_message(Message::Text(value.to_string().into()))
            .await?;
        Ok(elapsed_ms(started))
    }

    async fn send_pcm(&mut self, samples: &[f32]) -> Result<(), TranscriptionError> {
        if samples.is_empty() {
            return Ok(());
        }
        let bytes = pcm_i16_le(samples);
        let byte_count = bytes.len() as u64;
        let send_duration_ms = self
            .send_value(json!({
                "realtimeInput": {
                    "audio": {
                        "data": BASE64.encode(bytes),
                        "mimeType": "audio/pcm;rate=16000"
                    }
                }
            }))
            .await?;
        self.audio_chunks_sent += 1;
        self.bytes_sent += byte_count;
        self.context
            .observe_provider(ProviderTelemetryEvent::AudioChunkSent {
                duration_ms: samples.len() as u64 * 1_000 / u64::from(GEMINI_INPUT_SAMPLE_RATE),
                bytes: byte_count,
                send_duration_ms,
            });
        debug!(
            audio_chunk_duration_ms =
                samples.len() as u64 * 1_000 / u64::from(GEMINI_INPUT_SAMPLE_RATE),
            audio_chunks_sent = self.audio_chunks_sent,
            bytes_sent = self.bytes_sent,
            send_duration_ms,
            "Gemini Live audio chunk sent"
        );
        Ok(())
    }

    async fn push_samples(
        &mut self,
        samples: &[f32],
        flush: bool,
    ) -> Result<(), TranscriptionError> {
        for chunk in self.chunker.push(samples) {
            self.send_pcm(&chunk).await?;
        }
        if flush {
            if let Some(chunk) = self.chunker.flush() {
                self.send_pcm(&chunk).await?;
            }
        }
        Ok(())
    }

    async fn activity_start(&mut self, at: AudioTimestamp) -> Result<(), TranscriptionError> {
        let send_duration_ms = self
            .send_value(json!({ "realtimeInput": { "activityStart": {} } }))
            .await?;
        let activity_id = self.next_activity_id;
        self.next_activity_id += 1;
        let previous = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .begin(activity_id, at);
        if let Some(previous) = previous {
            if previous.text.trim().is_empty() {
                emit_error(
                    &self.context,
                    "Gemini Live input activity was superseded without a transcription".into(),
                );
            } else {
                emit_finalized(&self.context, previous, &self.sequence);
            }
        }
        self.context
            .emit(TranscriptionEvent::SpeechStarted(SpeechBoundary {
                session_id: self.context.session_id,
                transcription_session_id: self.context.transcription_session_id,
                source: self.context.source,
                provider: TranscriptionProviderId::GoogleGemini,
                at,
                provider_event_id: next_event_id(&self.context, &self.sequence, "speech-started"),
            }));
        self.context
            .observe_provider(ProviderTelemetryEvent::Configuration {
                automatic_vad_enabled: false,
                finalization_strategy: "activity_end_transcript_drain".into(),
            });
        self.context
            .observe_provider(ProviderTelemetryEvent::ActivityStartSent);
        info!(
            activity_id,
            activity_start_sent = true,
            send_duration_ms,
            automatic_vad_enabled = false,
            finalization_strategy = "activity_end_transcript_drain",
            "Gemini Live manual activity started"
        );
        self.state_changed.notify_one();
        Ok(())
    }

    async fn activity_end(&mut self, at: AudioTimestamp) -> Result<(), TranscriptionError> {
        self.push_samples(&[], true).await?;
        let send_duration_ms = self
            .send_value(json!({ "realtimeInput": { "activityEnd": {} } }))
            .await?;
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .end(Instant::now(), at);
        self.context
            .emit(TranscriptionEvent::SpeechEnded(SpeechBoundary {
                session_id: self.context.session_id,
                transcription_session_id: self.context.transcription_session_id,
                source: self.context.source,
                provider: TranscriptionProviderId::GoogleGemini,
                at,
                provider_event_id: next_event_id(&self.context, &self.sequence, "speech-ended"),
            }));
        self.context
            .observe_provider(ProviderTelemetryEvent::ActivityEndSent);
        info!(
            activity_end_sent = true,
            send_duration_ms,
            audio_chunks_sent = self.audio_chunks_sent,
            bytes_sent = self.bytes_sent,
            "Gemini Live manual activity ended after final audio chunk"
        );
        self.state_changed.notify_one();
        Ok(())
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

        if chunk.activity == AudioActivity::Start {
            self.activity_start(chunk.started_at).await?;
        }
        let active = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_active();
        if !chunk.samples.is_empty() {
            if !active {
                return Err(TranscriptionError::ProviderUnavailable(
                    "Gemini Live received audio outside a local VAD activity".into(),
                ));
            }
            self.state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .update_audio_end(chunk.ended_at);
            self.push_samples(&chunk.samples, false).await?;
        }
        if chunk.activity == AudioActivity::End {
            self.activity_end(chunk.ended_at).await?;
        }
        Ok(())
    }

    async fn finish(&mut self) -> Result<(), TranscriptionError> {
        self.ensure_open()?;
        let active = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_active();
        if active {
            let (_, ended_at) = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .current_timing();
            self.activity_end(ended_at).await?;
        }
        let wait = async {
            loop {
                if self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .is_idle()
                {
                    break;
                }
                self.final_notification.notified().await;
            }
        };
        let _ = tokio::time::timeout(self.finalization_timeout + Duration::from_millis(100), wait)
            .await;
        self.send_value(json!({ "realtimeInput": { "audioStreamEnd": true } }))
            .await?;
        self.closing.store(true, Ordering::Release);
        let close_result = self.writer.send_message(Message::Close(None)).await;
        self.closed.store(true, Ordering::Release);
        self.stop_receiver().await;
        close_result
    }

    async fn cancel(&mut self) -> Result<(), TranscriptionError> {
        if self.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        self.closing.store(true, Ordering::Release);
        let close_result = self.writer.send_message(Message::Close(None)).await;
        self.stop_receiver().await;
        close_result
    }
}

fn pcm_i16_le(samples: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        let value = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

struct ReceiverTask {
    context: TranscriptionSessionContext,
    reader: Box<dyn GeminiMessageStream>,
    cancellation: CancellationToken,
    closing: Arc<AtomicBool>,
    state: Arc<Mutex<InputTurnMachine>>,
    state_changed: Arc<tokio::sync::Notify>,
    final_notification: Arc<tokio::sync::Notify>,
    sequence: Arc<AtomicU64>,
}

async fn receive_messages(mut task: ReceiverTask) {
    loop {
        let deadline = task
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .next_deadline();
        tokio::select! {
            _ = task.cancellation.cancelled() => break,
            _ = task.state_changed.notified() => {},
            _ = sleep_until(deadline), if deadline.is_some() => {
                finalize_due(&task);
            }
            message = task.reader.next_message() => {
                let Some(message) = message else {
                    if !task.closing.load(Ordering::Acquire) {
                        emit_error(&task.context, "Gemini Live connection closed unexpectedly".into());
                    }
                    break;
                };
                match message {
                    Ok(message @ (Message::Text(_) | Message::Binary(_))) => {
                        match parse_json_message(&message) {
                            Ok(payload) => handle_server_payload(&task, &payload),
                            Err(error) => emit_error(
                                &task.context,
                                format!("invalid Gemini Live response: {error}"),
                            ),
                        }
                    }
                    Ok(Message::Close(_)) => {
                        if !task.closing.load(Ordering::Acquire) {
                            emit_error(&task.context, "Gemini Live connection closed unexpectedly".into());
                        }
                        break;
                    }
                    Err(error) => {
                        if !task.closing.load(Ordering::Acquire) {
                            emit_error(&task.context, format!("Gemini Live receive failed: {error}"));
                        }
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(deadline).await;
    } else {
        std::future::pending::<()>().await;
    }
}

fn handle_server_payload(task: &ReceiverTask, payload: &Value) {
    if let Some(error) = payload.get("error") {
        emit_error(&task.context, format!("Gemini Live server error: {error}"));
        return;
    }
    let Some(server_content) = payload.get("serverContent") else {
        return;
    };
    if let Some(fragment) = server_content
        .get("inputTranscription")
        .and_then(|value| value.get("text"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        let now = Instant::now();
        let update = task
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .update_transcript(fragment, now);
        if let Some((text, revision)) = update {
            task.context
                .observe_provider(ProviderTelemetryEvent::InputTranscriptionReceived);
            let (started_at, ended_at) = task
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .current_timing();
            task.context
                .emit(TranscriptionEvent::Partial(PartialTranscript(
                    TranscriptPayload {
                        session_id: task.context.session_id,
                        transcription_session_id: task.context.transcription_session_id,
                        source: task.context.source,
                        provider: TranscriptionProviderId::GoogleGemini,
                        language: None,
                        text,
                        started_at,
                        ended_at,
                        confidence: None,
                        is_final: false,
                        provider_event_id: next_event_id(&task.context, &task.sequence, "partial"),
                        segment_id: None,
                        processing_time_ms: None,
                    },
                )));
            info!(
                partial_revision = revision,
                input_transcription_received = true,
                "Gemini Live input transcription revision received"
            );
            task.state_changed.notify_one();
        }
    }
    if server_content
        .get("turnComplete")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        task.context
            .observe_provider(ProviderTelemetryEvent::ServerTurnCompleteReceived);
        info!(
            server_turn_complete_received = true,
            "Gemini Live model turn completed; input finalization remains local"
        );
    }
}

fn finalize_due(task: &ReceiverTask) {
    let finalized = task
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take_if_due(Instant::now());
    let Some(finalized) = finalized else {
        return;
    };
    if finalized.text.trim().is_empty() {
        emit_error(
            &task.context,
            "Gemini Live input activity ended without a transcription".into(),
        );
    } else {
        emit_finalized(&task.context, finalized, &task.sequence);
    }
    task.final_notification.notify_one();
}

fn emit_finalized(
    context: &TranscriptionSessionContext,
    finalized: FinalizedInput,
    sequence: &AtomicU64,
) {
    if finalized.text.trim().is_empty() {
        return;
    }
    info!(
        activity_id = finalized.activity_id,
        partial_revision_count = finalized.revision_count,
        activity_end_to_last_partial_ms = finalized
            .last_revision_at
            .map(|at| signed_millis(finalized.activity_end_at, at)),
        activity_end_to_final_transcript_ms = elapsed_tokio_ms(finalized.activity_end_at),
        finalization_reason = finalized.reason.as_str(),
        local_final_transcript_emitted = true,
        "Gemini Live input transcript finalized locally"
    );
    context.observe_provider(ProviderTelemetryEvent::LocalFinalTranscriptEmitted {
        finalization_reason: finalized.reason.as_str().into(),
    });
    context.emit(TranscriptionEvent::Final(FinalTranscript(
        TranscriptPayload {
            session_id: context.session_id,
            transcription_session_id: context.transcription_session_id,
            source: context.source,
            provider: TranscriptionProviderId::GoogleGemini,
            language: None,
            text: finalized.text,
            started_at: finalized.started_at,
            ended_at: finalized.ended_at,
            confidence: None,
            is_final: true,
            provider_event_id: next_event_id(context, sequence, "final"),
            segment_id: None,
            processing_time_ms: Some(elapsed_tokio_ms(finalized.activity_end_at)),
        },
    )));
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
    warn!(%message, "Gemini Live transcription error");
    context.emit(TranscriptionEvent::Error(TranscriptionErrorEvent {
        session_id: context.session_id,
        transcription_session_id: context.transcription_session_id,
        source: context.source,
        provider: TranscriptionProviderId::GoogleGemini,
        message,
        recoverable: true,
    }));
}

fn elapsed_ms(start: StdInstant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn elapsed_tokio_ms(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn signed_millis(from: Instant, to: Instant) -> i64 {
    if let Some(duration) = to.checked_duration_since(from) {
        i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
    } else {
        -i64::try_from(from.duration_since(to).as_millis()).unwrap_or(i64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::types::{AudioSource, CaptureStreamId};
    use crate::conversation::SessionId;
    use crate::transcription::session::TranscriptionSessionId;
    use crate::transcription::types::TranscriptionLanguage;

    struct FakeSink {
        sent: Arc<Mutex<Vec<Message>>>,
    }

    struct DelayedFakeSink {
        delay: Duration,
    }

    #[async_trait]
    impl GeminiMessageSink for DelayedFakeSink {
        async fn send_message(&mut self, _message: Message) -> Result<(), TranscriptionError> {
            tokio::time::sleep(self.delay).await;
            Ok(())
        }
    }

    #[async_trait]
    impl GeminiMessageSink for FakeSink {
        async fn send_message(&mut self, message: Message) -> Result<(), TranscriptionError> {
            self.sent.lock().unwrap().push(message);
            Ok(())
        }
    }

    struct FakeStream {
        receiver: tokio::sync::mpsc::UnboundedReceiver<Result<Message, String>>,
    }

    #[async_trait]
    impl GeminiMessageStream for FakeStream {
        async fn next_message(&mut self) -> Option<Result<Message, String>> {
            self.receiver.recv().await
        }
    }

    type FakeSessionParts = (
        GeminiLiveSession,
        Arc<Mutex<Vec<Message>>>,
        tokio::sync::mpsc::UnboundedSender<Result<Message, String>>,
        Arc<Mutex<Vec<TranscriptionEvent>>>,
    );

    fn fake_session() -> FakeSessionParts {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&events);
        let (server, receiver) = tokio::sync::mpsc::unbounded_channel();
        let context = TranscriptionSessionContext {
            session_id: SessionId::from_value(7),
            transcription_session_id: TranscriptionSessionId::next(),
            source: AudioSource::SystemOutput,
            language: TranscriptionLanguage::Automatic,
            model: Some("test-model".into()),
            streaming_audio_config: Some(config()),
            sink: Arc::new(move |event| captured.lock().unwrap().push(event)),
            provider_telemetry: Arc::new(|_| {}),
        };
        let session = GeminiLiveSession::new_with_transport(
            context,
            Box::new(FakeSink {
                sent: Arc::clone(&sent),
            }),
            Box::new(FakeStream { receiver }),
        );
        (session, sent, server, events)
    }

    fn chunk(activity: AudioActivity, samples: Vec<f32>, start: u64, end: u64) -> AudioChunk {
        AudioChunk {
            source: AudioSource::SystemOutput,
            capture_stream_id: CaptureStreamId::UNASSIGNED,
            sequence_number: start,
            samples,
            sample_rate: 16_000,
            started_at: AudioTimestamp(start),
            ended_at: AudioTimestamp(end),
            segment_id: None,
            activity,
            activity_observed_at: None,
        }
    }

    #[test]
    fn pcm_conversion_is_signed_16_bit_little_endian() {
        assert_eq!(pcm_i16_le(&[-2.0, 0.0, 2.0]), vec![1, 128, 0, 0, 255, 127]);
    }

    #[test]
    fn setup_json_parser_accepts_text_and_binary_frames() {
        let expected = json!({"setupComplete": {}});
        for message in [
            Message::Text(expected.to_string().into()),
            Message::Binary(expected.to_string().into_bytes().into()),
        ] {
            assert_eq!(parse_json_message(&message).unwrap(), expected);
        }
    }

    #[test]
    fn snapshot_revisions_replace_instead_of_concatenating() {
        let mut text = String::new();
        for revision in ["Como", "Como você", "Como você projetaria"] {
            text = merge_input_transcription(&text, revision);
        }
        assert_eq!(text, "Como você projetaria");
    }

    #[test]
    fn real_deltas_are_merged_with_word_boundaries() {
        let mut text = String::new();
        for delta in ["Como", "você", "projetaria", "?"] {
            text = merge_input_transcription(&text, delta);
        }
        assert_eq!(text, "Como você projetaria?");
    }

    #[test]
    fn overlapping_deltas_do_not_duplicate_samples_of_text() {
        assert_eq!(
            merge_input_transcription("microserv", "serviços"),
            "microserviços"
        );
    }

    #[test]
    fn one_hundred_ms_is_split_into_two_40ms_chunks_and_one_flushed_tail() {
        let samples: Vec<f32> = (0..1_600).map(|sample| sample as f32).collect();
        let mut chunker = PcmChunker::new(640);
        let mut chunks = chunker.push(&samples);
        assert_eq!(chunks.iter().map(Vec::len).collect::<Vec<_>>(), [640, 640]);
        chunks.push(chunker.flush().unwrap());
        assert_eq!(
            chunks.iter().map(Vec::len).collect::<Vec<_>>(),
            [640, 640, 320]
        );
        assert_eq!(chunks.into_iter().flatten().collect::<Vec<_>>(), samples);
    }

    #[test]
    fn chunker_keeps_order_across_capture_frame_boundaries_without_duplicates() {
        let samples: Vec<f32> = (0..2_123).map(|sample| sample as f32).collect();
        let mut chunker = PcmChunker::new(640);
        let mut chunks = Vec::new();
        chunks.extend(chunker.push(&samples[..137]));
        chunks.extend(chunker.push(&samples[137..1_001]));
        chunks.extend(chunker.push(&samples[1_001..]));
        chunks.push(chunker.flush().unwrap());
        assert_eq!(chunks.into_iter().flatten().collect::<Vec<_>>(), samples);
    }

    #[tokio::test]
    async fn fake_transport_orders_activity_start_audio_tail_and_activity_end() {
        let (mut session, sent, _server, _events) = fake_session();
        session
            .push_audio(chunk(AudioActivity::Start, Vec::new(), 0, 0))
            .await
            .unwrap();
        session
            .push_audio(chunk(AudioActivity::None, vec![0.25; 1_600], 0, 100))
            .await
            .unwrap();
        session
            .push_audio(chunk(AudioActivity::End, Vec::new(), 100, 100))
            .await
            .unwrap();

        let payloads: Vec<Value> = sent
            .lock()
            .unwrap()
            .iter()
            .filter_map(|message| parse_json_message(message).ok())
            .collect();
        assert!(payloads[0]["realtimeInput"].get("activityStart").is_some());
        assert_eq!(
            payloads
                .iter()
                .filter(|payload| payload["realtimeInput"].get("audio").is_some())
                .count(),
            3,
            "100ms becomes 40ms + 40ms + flushed 20ms"
        );
        assert!(payloads.last().unwrap()["realtimeInput"]
            .get("activityEnd")
            .is_some());
        session.cancel().await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn fake_transport_emits_partial_then_final_without_turn_complete() {
        let (mut session, _sent, server, events) = fake_session();
        session
            .push_audio(chunk(AudioActivity::Start, Vec::new(), 0, 0))
            .await
            .unwrap();
        session
            .push_audio(chunk(AudioActivity::None, vec![0.25; 640], 0, 40))
            .await
            .unwrap();
        server
            .send(Ok(Message::Text(
                json!({"serverContent":{"inputTranscription":{"text":"Como você"}}})
                    .to_string()
                    .into(),
            )))
            .unwrap();
        tokio::task::yield_now().await;
        session
            .push_audio(chunk(AudioActivity::End, Vec::new(), 40, 40))
            .await
            .unwrap();
        tokio::time::advance(Duration::from_millis(301)).await;
        tokio::task::yield_now().await;

        let events = events.lock().unwrap();
        let partial = events
            .iter()
            .position(|event| matches!(event, TranscriptionEvent::Partial(_)))
            .unwrap();
        let final_event = events
            .iter()
            .position(|event| matches!(event, TranscriptionEvent::Final(_)))
            .unwrap();
        assert!(partial < final_event);
        drop(events);
        session.cancel().await.unwrap();
    }

    #[tokio::test]
    async fn a_silent_receiver_never_blocks_continuous_sending_or_cancel() {
        let (mut session, sent, _server, _events) = fake_session();
        session
            .push_audio(chunk(AudioActivity::Start, Vec::new(), 0, 0))
            .await
            .unwrap();
        for index in 0..10 {
            session
                .push_audio(chunk(
                    AudioActivity::None,
                    vec![0.25; 640],
                    index * 40,
                    (index + 1) * 40,
                ))
                .await
                .unwrap();
        }
        assert_eq!(sent.lock().unwrap().len(), 11);
        session.cancel().await.unwrap();
        assert!(session.receiver.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn late_turn_complete_never_duplicates_a_local_final() {
        let (mut session, _sent, server, events) = fake_session();
        session
            .push_audio(chunk(AudioActivity::Start, Vec::new(), 0, 0))
            .await
            .unwrap();
        server
            .send(Ok(Message::Text(
                json!({"serverContent":{"inputTranscription":{"text":"uma fala"}}})
                    .to_string()
                    .into(),
            )))
            .unwrap();
        tokio::task::yield_now().await;
        session
            .push_audio(chunk(AudioActivity::End, Vec::new(), 100, 100))
            .await
            .unwrap();
        tokio::time::advance(Duration::from_millis(301)).await;
        tokio::task::yield_now().await;
        server
            .send(Ok(Message::Text(
                json!({"serverContent":{"turnComplete":true}})
                    .to_string()
                    .into(),
            )))
            .unwrap();
        tokio::task::yield_now().await;
        assert_eq!(
            events
                .lock()
                .unwrap()
                .iter()
                .filter(|event| matches!(event, TranscriptionEvent::Final(_)))
                .count(),
            1
        );
        session.cancel().await.unwrap();
    }

    #[tokio::test]
    async fn a_slow_sender_is_visible_in_provider_telemetry() {
        let observations = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&observations);
        let (_server, receiver) = tokio::sync::mpsc::unbounded_channel();
        let context = TranscriptionSessionContext {
            session_id: SessionId::from_value(8),
            transcription_session_id: TranscriptionSessionId::next(),
            source: AudioSource::SystemOutput,
            language: TranscriptionLanguage::Automatic,
            model: Some("test-model".into()),
            streaming_audio_config: Some(config()),
            sink: Arc::new(|_| {}),
            provider_telemetry: Arc::new(move |event| captured.lock().unwrap().push(event)),
        };
        let mut session = GeminiLiveSession::new_with_transport(
            context,
            Box::new(DelayedFakeSink {
                delay: Duration::from_millis(20),
            }),
            Box::new(FakeStream { receiver }),
        );
        session
            .push_audio(chunk(AudioActivity::Start, Vec::new(), 0, 0))
            .await
            .unwrap();
        session
            .push_audio(chunk(AudioActivity::None, vec![0.25; 640], 0, 40))
            .await
            .unwrap();
        assert!(observations.lock().unwrap().iter().any(|event| matches!(
            event,
            ProviderTelemetryEvent::AudioChunkSent {
                send_duration_ms,
                ..
            } if *send_duration_ms >= 15
        )));
        session.cancel().await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires HELPPYE_GEMINI_SMOKE_API_KEY and network"]
    async fn live_manual_activity_setup_smoke_test() {
        let key = std::env::var("HELPPYE_GEMINI_SMOKE_API_KEY").expect("sandbox API key");
        let provider = GeminiLiveTranscriptionProvider::with_api_key_loader(Arc::new(move || {
            Ok(Some(key.clone()))
        }));
        let context = TranscriptionSessionContext {
            session_id: SessionId::from_value(77),
            transcription_session_id: TranscriptionSessionId::next(),
            source: AudioSource::SystemOutput,
            language: TranscriptionLanguage::Automatic,
            model: Some("gemini-3.1-flash-live-preview".into()),
            streaming_audio_config: Some(config()),
            sink: Arc::new(|_| {}),
            provider_telemetry: Arc::new(|_| {}),
        };
        let mut session = provider.start_session(context).await.unwrap();
        session.cancel().await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires HELPPYE_GEMINI_SMOKE_API_KEY, HELPPYE_GEMINI_SMOKE_AUDIO and network"]
    async fn live_transcription_latency_smoke_test() {
        let key = std::env::var("HELPPYE_GEMINI_SMOKE_API_KEY").expect("sandbox API key");
        let audio_path = std::env::var("HELPPYE_GEMINI_SMOKE_AUDIO").expect("PCM fixture");
        let bytes = std::fs::read(audio_path).unwrap();
        let samples: Vec<f32> = bytes
            .chunks_exact(2)
            .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]) as f32 / i16::MAX as f32)
            .collect();
        let provider = GeminiLiveTranscriptionProvider::with_api_key_loader(Arc::new(move || {
            Ok(Some(key.clone()))
        }));
        let observations = Arc::new(Mutex::new(Vec::<(TranscriptionEvent, StdInstant)>::new()));
        let captured = Arc::clone(&observations);
        let provider_observations = Arc::new(Mutex::new(
            Vec::<(ProviderTelemetryEvent, StdInstant)>::new(),
        ));
        let captured_provider = Arc::clone(&provider_observations);
        let final_ready = Arc::new(tokio::sync::Notify::new());
        let notify = Arc::clone(&final_ready);
        let context = TranscriptionSessionContext {
            session_id: SessionId::from_value(78),
            transcription_session_id: TranscriptionSessionId::next(),
            source: AudioSource::SystemOutput,
            language: TranscriptionLanguage::Automatic,
            model: Some("gemini-3.1-flash-live-preview".into()),
            streaming_audio_config: Some(config()),
            sink: Arc::new(move |event| {
                if matches!(event, TranscriptionEvent::Final(_)) {
                    notify.notify_one();
                }
                captured.lock().unwrap().push((event, StdInstant::now()));
            }),
            provider_telemetry: Arc::new(move |event| {
                captured_provider
                    .lock()
                    .unwrap()
                    .push((event, StdInstant::now()));
            }),
        };
        let mut session = provider.start_session(context).await.unwrap();
        let speech_start = StdInstant::now();
        session
            .push_audio(chunk(AudioActivity::Start, Vec::new(), 0, 0))
            .await
            .unwrap();
        for (index, audio) in samples.chunks(640).enumerate() {
            let start = index as u64 * 40;
            session
                .push_audio(chunk(
                    AudioActivity::None,
                    audio.to_vec(),
                    start,
                    start + audio.len() as u64 * 1_000 / 16_000,
                ))
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(audio.len() as u64 * 1_000 / 16_000)).await;
        }
        let speech_end = StdInstant::now();
        let end_ms = samples.len() as u64 * 1_000 / 16_000;
        session
            .push_audio(chunk(AudioActivity::End, Vec::new(), end_ms, end_ms))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), final_ready.notified())
            .await
            .expect("final transcript within five seconds");
        let observations = observations.lock().unwrap();
        let first_partial_at = observations
            .iter()
            .find(|(event, _)| matches!(event, TranscriptionEvent::Partial(_)))
            .map(|(_, at)| *at);
        let first_partial_relative_to_speech_end = first_partial_at.map(|at| {
            if at >= speech_end {
                at.duration_since(speech_end).as_millis() as i128
            } else {
                -(speech_end.duration_since(at).as_millis() as i128)
            }
        });
        let (final_event, final_at) = observations
            .iter()
            .find(|(event, _)| matches!(event, TranscriptionEvent::Final(_)))
            .unwrap();
        let latency_ms = final_at.duration_since(speech_end).as_millis() as u64;
        let activity_end_sent_at = provider_observations
            .lock()
            .unwrap()
            .iter()
            .find(|(event, _)| matches!(event, ProviderTelemetryEvent::ActivityEndSent))
            .map(|(_, at)| *at)
            .unwrap();
        let TranscriptionEvent::Final(final_transcript) = final_event else {
            unreachable!();
        };
        println!(
            "speech_start_to_first_partial_ms={:?} first_partial_relative_to_speech_end_ms={first_partial_relative_to_speech_end:?} speech_end_to_activity_end_ms={} activity_end_to_final_ms={} speech_end_to_final_ms={latency_ms} final={:?}",
            first_partial_at.map(|at| at.duration_since(speech_start).as_millis()),
            activity_end_sent_at.duration_since(speech_end).as_millis(),
            final_at.duration_since(activity_end_sent_at).as_millis(),
            final_transcript.text
        );
        assert!(!final_transcript.text.trim().is_empty());
        assert!(latency_ms <= 1_500, "measured {latency_ms}ms");
        drop(observations);
        session.cancel().await.unwrap();
    }

    fn config() -> StreamingAudioConfig {
        StreamingAudioConfig {
            manual_activity_detection: true,
            activity_end_silence_ms: 600,
            target_chunk_ms: 40,
            transcript_drain_ms: 300,
            finalization_timeout_ms: 1_500,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn final_does_not_depend_on_turn_complete() {
        let mut machine = InputTurnMachine::new(config());
        let now = Instant::now();
        machine.begin(1, AudioTimestamp(0));
        machine.update_transcript("texto", now);
        machine.end(now, AudioTimestamp(500));
        tokio::time::advance(Duration::from_millis(301)).await;
        let finalization = machine.take_if_due(Instant::now()).unwrap();
        assert_eq!(finalization.text, "texto");
        assert_eq!(
            finalization.reason,
            FinalizationReason::TranscriptStabilized
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_new_partial_restarts_the_drain_timer() {
        let mut machine = InputTurnMachine::new(config());
        let now = Instant::now();
        machine.begin(1, AudioTimestamp(0));
        machine.update_transcript("Como", now);
        machine.end(now, AudioTimestamp(500));
        tokio::time::advance(Duration::from_millis(250)).await;
        machine.update_transcript("Como você", Instant::now());
        tokio::time::advance(Duration::from_millis(100)).await;
        assert!(machine.take_if_due(Instant::now()).is_none());
        tokio::time::advance(Duration::from_millis(201)).await;
        assert_eq!(
            machine.take_if_due(Instant::now()).unwrap().text,
            "Como você"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn defensive_timeout_produces_the_best_text() {
        let mut machine = InputTurnMachine::new(config());
        let now = Instant::now();
        machine.begin(1, AudioTimestamp(0));
        machine.update_transcript("melhor texto", now);
        machine.end(now, AudioTimestamp(500));
        tokio::time::advance(Duration::from_millis(1_501)).await;
        let finalization = machine.take_if_due(Instant::now()).unwrap();
        assert_eq!(finalization.text, "melhor texto");
        assert_eq!(
            finalization.reason,
            FinalizationReason::ProviderDrainTimeout
        );
    }

    #[tokio::test(start_paused = true)]
    async fn no_text_waits_for_the_absolute_timeout() {
        let mut machine = InputTurnMachine::new(config());
        let now = Instant::now();
        machine.begin(1, AudioTimestamp(0));
        machine.end(now, AudioTimestamp(500));
        tokio::time::advance(Duration::from_millis(1_499)).await;
        assert!(machine.take_if_due(Instant::now()).is_none());
        tokio::time::advance(Duration::from_millis(2)).await;
        assert!(machine.take_if_due(Instant::now()).unwrap().text.is_empty());
    }

    #[test]
    fn late_updates_after_final_are_ignored() {
        let mut machine = InputTurnMachine::new(config());
        let now = Instant::now();
        machine.begin(1, AudioTimestamp(0));
        machine.update_transcript("texto", now);
        machine.end(now, AudioTimestamp(500));
        let finalization = machine.take_now(FinalizationReason::TranscriptStabilized);
        assert!(finalization.is_some());
        assert!(machine.update_transcript("duplicata", now).is_none());
    }

    #[test]
    fn next_activity_can_start_after_final() {
        let mut machine = InputTurnMachine::new(config());
        let now = Instant::now();
        machine.begin(1, AudioTimestamp(0));
        machine.end(now, AudioTimestamp(500));
        machine.take_now(FinalizationReason::ProviderDrainTimeout);
        assert!(machine.begin(2, AudioTimestamp(1_000)).is_none());
        assert!(machine.is_active());
    }
}
