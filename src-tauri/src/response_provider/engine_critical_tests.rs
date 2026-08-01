use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures_util::stream;
use tauri::Listener;

use super::*;
use crate::audio::segment::{AudioTimestamp, SegmentId};
use crate::response_provider::provider::{ResponseProviderId, ResponseStream, ResponseStreamMeta};

struct SequenceProvider {
    replies: Mutex<VecDeque<String>>,
    requests: Mutex<Vec<ResponseRequest>>,
    count: AtomicUsize,
}

impl SequenceProvider {
    fn new(replies: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.iter().map(|value| value.to_string()).collect()),
            requests: Mutex::new(Vec::new()),
            count: AtomicUsize::new(0),
        })
    }
}

#[async_trait]
impl ResponseProvider for SequenceProvider {
    fn id(&self) -> ResponseProviderId {
        ResponseProviderId::Misconfigured
    }

    fn capabilities(&self) -> ResponseProviderCapabilities {
        ResponseProviderCapabilities::none()
    }

    fn provider_name(&self) -> &'static str {
        "sequence"
    }

    async fn stream_reply(
        &self,
        request: ResponseRequest,
    ) -> Result<(ResponseStream, ResponseStreamMeta), ResponseProviderError> {
        self.count.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(request);
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("test reply missing");
        Ok((
            Box::pin(stream::iter(vec![Ok(ResponseChunk::Delta(reply))])),
            ResponseStreamMeta { http_status: 200 },
        ))
    }
}

fn current(id: u64, revision: u64, text: &str) -> ConversationUtterance {
    ConversationUtterance {
        id: UtteranceId::from_raw(id),
        speaker: ConversationSpeaker::OtherPerson,
        source: AudioSource::SystemOutput,
        text: text.to_string(),
        raw_text: text.to_string(),
        segments: vec![SegmentId::next()],
        received_sequence: id,
        started_at: AudioTimestamp(id * 1_000),
        ended_at: AudioTimestamp(id * 1_000 + 500),
        finalized_at: Some(AudioTimestamp(id * 1_000 + 500)),
        revision,
        transcription_completed_at: Instant::now(),
        speech_ended_at: Instant::now(),
    }
}

fn turn(utterance: &ConversationUtterance) -> ConversationTurn {
    ConversationTurn {
        id: TurnId::from_raw(utterance.id.value()),
        speaker: utterance.speaker,
        source: utterance.source,
        text: utterance.text.clone(),
        raw_text: utterance.raw_text.clone(),
        utterances: vec![utterance.id],
        started_at: utterance.started_at,
        ended_at: utterance.ended_at,
        finalized_at: None,
    }
}

fn trigger(session_id: SessionId, utterance: ConversationUtterance) -> GenerationTrigger {
    let now = Instant::now();
    GenerationTrigger {
        session_id,
        utterance_id: utterance.id,
        utterance_revision: utterance.revision,
        utterance_text: utterance.text.clone(),
        utterance,
        utterance_finalized_at: now,
        speech_ended_at: now,
        automatic: true,
        finalization_reason: "inactivity_timeout".to_string(),
        gap_ms_used: 1_800,
        silence_detected_ms: Some(1_800),
    }
}

async fn next_event(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>,
    kind: &str,
) -> serde_json::Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = receiver.recv().await.expect("event stream ended");
            if event["type"] == kind {
                return event;
            }
        }
    })
    .await
    .expect("event timeout")
}

fn capture(
    app: &tauri::AppHandle<tauri::test::MockRuntime>,
) -> tokio::sync::mpsc::UnboundedReceiver<serde_json::Value> {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    app.listen_any(
        super::super::events::RESPONSE_SUGGESTION_EVENT,
        move |event| {
            if let Ok(value) = serde_json::from_str(event.payload()) {
                let _ = sender.send(value);
            }
        },
    );
    receiver
}

#[tokio::test]
async fn punctuation_is_never_published_and_one_retry_can_repair_it() {
    let provider = SequenceProvider::new(&[
        ".",
        "A prata e o metal puro com maior condutividade eletrica a temperatura ambiente.",
    ]);
    let engine = ResponseEngine::for_test(provider.clone());
    let session = engine.active_session_id();
    let app = tauri::test::mock_app();
    let handle = app.handle().clone();
    let mut events = capture(&handle);
    let utterance = current(
        1,
        1,
        "Qual metal puro tem a maior condutividade eletrica a temperatura ambiente?",
    );

    engine
        .clone()
        .trigger_generation(handle, turn(&utterance), trigger(session, utterance));

    let completed = next_event(&mut events, "completed").await;
    assert!(completed["text"].as_str().unwrap().contains("prata"));
    assert_eq!(provider.count.load(Ordering::SeqCst), 2);
    let requests = provider.requests.lock().unwrap();
    let repair = &requests[1];
    let repair_text = repair
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!repair_text.contains("A resposta anterior: ."));
    assert!(!repair_text.contains("CONTEXTO ANTERIOR"));
}

#[tokio::test]
async fn two_invalid_attempts_end_as_invalid_without_a_delta() {
    let provider = SequenceProvider::new(&[".", "?"]);
    let engine = ResponseEngine::for_test(provider.clone());
    let session = engine.active_session_id();
    let app = tauri::test::mock_app();
    let handle = app.handle().clone();
    let mut events = capture(&handle);
    let utterance = current(
        1,
        1,
        "Explique consistencia estrita em alta disponibilidade",
    );

    engine
        .clone()
        .trigger_generation(handle, turn(&utterance), trigger(session, utterance));

    let invalid = next_event(&mut events, "invalid").await;
    assert_eq!(invalid["failure"], "punctuation_only");
    assert_eq!(provider.count.load(Ordering::SeqCst), 2);
    while let Ok(event) = events.try_recv() {
        assert_ne!(event["type"], "delta");
    }
}

#[tokio::test]
async fn stale_automatic_input_never_reaches_the_provider() {
    let provider = SequenceProvider::new(&["resposta que nao deve ser usada"]);
    let engine = ResponseEngine::for_test(provider.clone());
    let session = engine.active_session_id();
    let app = tauri::test::mock_app();
    let utterance = current(1, 1, "pergunta antiga");
    let mut old_trigger = trigger(session, utterance.clone());
    old_trigger.speech_ended_at = Instant::now() - Duration::from_secs(11);

    engine
        .clone()
        .trigger_generation(app.handle().clone(), turn(&utterance), old_trigger);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(provider.count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn noisy_prior_user_speech_cannot_become_the_high_availability_answer() {
    let leaked = "Vou te mandar uma pergunta relacionada a area de tecnologia, ate ligada.";
    let provider = SequenceProvider::new(&[
        leaked,
        "Eu distribuiria tres replicas por zonas independentes, exigindo quorum para commits e failover sem perder consistencia.",
    ]);
    let engine = ResponseEngine::for_test(provider.clone());
    let session = engine.active_session_id();
    engine.push_history(
        session,
        ConversationUtterance {
            id: UtteranceId::from_raw(1),
            speaker: ConversationSpeaker::User,
            source: AudioSource::Microphone,
            text: leaked.to_string(),
            raw_text: leaked.to_string(),
            segments: vec![SegmentId::next()],
            received_sequence: 1,
            started_at: AudioTimestamp(0),
            ended_at: AudioTimestamp(500),
            finalized_at: Some(AudioTimestamp(500)),
            revision: 1,
            transcription_completed_at: Instant::now(),
            speech_ended_at: Instant::now(),
        },
    );
    let app = tauri::test::mock_app();
    let handle = app.handle().clone();
    let mut events = capture(&handle);
    let utterance = current(
        2,
        1,
        "Como voce projetaria um sistema de alta disponibilidade para resistir a queda simultanea de duas zonas garantindo consistencia estrita?",
    );
    engine
        .clone()
        .trigger_generation(handle, turn(&utterance), trigger(session, utterance));

    let completed = next_event(&mut events, "completed").await;
    let answer = completed["text"].as_str().unwrap();
    assert!(answer.contains("replicas"));
    assert!(!answer.contains("Vou te mandar uma pergunta"));
    assert_eq!(provider.count.load(Ordering::SeqCst), 2);
}
