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
        capture_stream_id: crate::audio::types::CaptureStreamId::UNASSIGNED,
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
        capture_stream_id: crate::audio::types::CaptureStreamId::UNASSIGNED,
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
            capture_stream_id: crate::audio::types::CaptureStreamId::UNASSIGNED,
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

// ---------------------------------------------------------------------------
// Fio de ponta a ponta: `ConversationTimeline` real (com o timer dedicado de
// utterance) até o `ResponseEngine`, exatamente como `lib.rs` conecta os dois via
// `set_event_sink`. Todos os testes acima disparam `trigger_generation` ou
// `process_conversation_events` diretamente com um `ConversationTimelineEvent`
// já pronto — nenhum exercitava o caminho que a produção realmente usa: captura
// → `ConversationTimeline::ingest_normalized_transcript` → timer de silêncio →
// sink → `process_conversation_events`. É exatamente essa lacuna que escondia o
// defeito relatado (utterance finalizada, turno elegível, nenhuma geração).
mod end_to_end_trigger {
    use std::time::Duration;

    use super::{next_event, ResponseEngine, SequenceProvider};
    use crate::audio::segment::{AudioTimestamp, SegmentId};
    use crate::audio::types::{AudioSource, CaptureStreamId};
    use crate::conversation::{ConversationAssemblerConfig, ConversationTimeline, SessionId};
    use crate::normalization::TranscriptNormalizationResult;
    use crate::response_provider::engine::process_conversation_events;
    use crate::transcription::envelope::TranscriptionResultEnvelope;
    use crate::transcription::events::{FinalTranscript, ProviderEventId, TranscriptPayload};
    use crate::transcription::provider::TranscriptionProviderId;
    use crate::transcription::session::TranscriptionSessionId;

    /// `same_speaker_utterance_gap_ms` curto o suficiente para o teste não esperar quase 2s
    /// por chamada, longo o suficiente para não competir com o agendamento do runtime.
    const TEST_GAP_MS: u64 = 60;

    fn envelope_and_transcript(
        session_id: SessionId,
        source: AudioSource,
        stream: CaptureStreamId,
        sequence: u64,
        text: &str,
        start: u64,
        end: u64,
    ) -> (TranscriptionResultEnvelope, FinalTranscript) {
        let envelope = TranscriptionResultEnvelope {
            session_id,
            segment_id: SegmentId::next(),
            source,
            capture_stream_id: stream,
            sequence_number: sequence,
            raw_text: text.to_string(),
            normalized_text: text.to_string(),
        };
        let transcript = FinalTranscript(TranscriptPayload {
            session_id,
            transcription_session_id: TranscriptionSessionId::next(),
            source,
            provider: TranscriptionProviderId::WhisperLocal,
            language: Some("pt".into()),
            text: text.to_string(),
            started_at: AudioTimestamp(start),
            ended_at: AudioTimestamp(end),
            confidence: Some(0.9),
            is_final: true,
            provider_event_id: ProviderEventId::new(&format!("evt-{sequence}")),
            segment_id: None,
            processing_time_ms: Some(12),
        });
        (envelope, transcript)
    }

    /// Espelha exatamente a conexão que `lib.rs` registra em produção via
    /// `timeline.set_event_sink`: toda finalização (reativa ou pelo timer dedicado) chega
    /// ao `ResponseEngine` pelo mesmo caminho, sem passar por nenhum atalho de teste.
    fn wire_timeline_to_engine(
        timeline: &std::sync::Arc<ConversationTimeline>,
        engine: std::sync::Arc<ResponseEngine>,
        app: tauri::AppHandle<tauri::test::MockRuntime>,
    ) {
        timeline.set_event_sink(std::sync::Arc::new(move |events| {
            process_conversation_events(&app, engine.clone(), &events);
        }));
    }

    #[tokio::test]
    async fn a_remote_utterance_finalized_by_the_dedicated_timer_reaches_the_response_engine() {
        let provider = SequenceProvider::new(&["resposta sugerida"]);
        let engine = ResponseEngine::for_test(provider.clone());

        let mut config = ConversationAssemblerConfig::default();
        config.same_speaker_utterance_gap_ms = TEST_GAP_MS;
        let timeline = std::sync::Arc::new(ConversationTimeline::new(config));
        timeline.attach();
        // Mesmo alinhamento de fronteira que `lib.rs` faz no boot
        // (`response_engine.begin_session(timeline.session_id())`): a sessão que importa
        // é a da timeline, não a que `ResponseEngine::for_test` cunhou sozinho.
        let session = timeline.session_id();
        engine.begin_session(session);

        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let mut suggestion_events = super::capture(&handle);
        wire_timeline_to_engine(&timeline, engine.clone(), handle);

        let stream = CaptureStreamId::next();
        let (envelope, transcript) = envelope_and_transcript(
            session,
            AudioSource::SystemOutput,
            stream,
            1,
            "em qual situação você usaria monolitos?",
            0,
            1_500,
        );
        let normalization = TranscriptNormalizationResult::unchanged(transcript.0.text.clone());
        timeline.ingest_normalized_transcript(
            &envelope,
            &transcript,
            &normalization,
            std::time::Instant::now(),
        );

        // O turno permanece aberto — a utterance finaliza só por silêncio, via o timer
        // dedicado, sem nenhum outro segmento, flush ou stop de captura.
        let started = next_event(&mut suggestion_events, "started").await;
        assert_eq!(started["session_id"], session.value());
        let completed = next_event(&mut suggestion_events, "completed").await;
        assert_eq!(completed["text"], "resposta sugerida");
    }

    #[tokio::test]
    async fn a_remote_utterance_inside_a_still_open_turn_is_eligible_for_generation() {
        let provider = SequenceProvider::new(&["resposta sugerida"]);
        let engine = ResponseEngine::for_test(provider.clone());

        let mut config = ConversationAssemblerConfig::default();
        config.same_speaker_utterance_gap_ms = TEST_GAP_MS;
        let timeline = std::sync::Arc::new(ConversationTimeline::new(config));
        timeline.attach();
        let session = timeline.session_id();
        engine.begin_session(session);

        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let mut suggestion_events = super::capture(&handle);
        wire_timeline_to_engine(&timeline, engine.clone(), handle);

        let stream = CaptureStreamId::next();
        for (index, question) in [
            "em qual situação você usaria monolitos?",
            "em qual situação você usaria microserviços?",
        ]
        .iter()
        .enumerate()
        {
            let (envelope, transcript) = envelope_and_transcript(
                session,
                AudioSource::SystemOutput,
                stream,
                index as u64 + 1,
                question,
                index as u64 * 2_000,
                index as u64 * 2_000 + 1_500,
            );
            let normalization = TranscriptNormalizationResult::unchanged(transcript.0.text.clone());
            timeline.ingest_normalized_transcript(
                &envelope,
                &transcript,
                &normalization,
                std::time::Instant::now(),
            );
            // Gap curto entre os dois segmentos: continuam a mesma utterance, não
            // disparam o timer entre eles.
            tokio::time::sleep(Duration::from_millis(TEST_GAP_MS / 3)).await;
        }

        let snapshot = timeline.snapshot();
        assert!(
            snapshot
                .turns
                .iter()
                .any(|turn| turn.finalized_at.is_none()),
            "o turno segue aberto: só a utterance finaliza por silêncio"
        );

        let started = next_event(&mut suggestion_events, "started").await;
        assert_eq!(started["session_id"], session.value());
        next_event(&mut suggestion_events, "completed").await;
    }
}
