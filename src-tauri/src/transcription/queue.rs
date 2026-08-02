//! Bounded per-source queues decouple segment production from transcription. Producers
//! always use `try_send`: capture never waits for a model or network provider. Teardown
//! is a command in the same lane, so `finish_source` cannot overtake queued audio.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Serialize;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

use crate::audio::segment::AudioSegment;
use crate::audio::types::AudioSource;
use crate::integrity::{IntegrityStage, SourceIntegrityError};
use crate::transcription::envelope::{MonotonicTimestamp, TranscriptionWorkItem};
use crate::transcription::runtime::TranscriptionRuntime;
use crate::transcription::session::AudioChunk;

/// Small on purpose: segments already represent multiple seconds of speech each, so a
/// deep backlog means transcription has fallen behind real time.
pub const QUEUE_CAPACITY: usize = 16;

#[derive(Default)]
struct LaneMetrics {
    enqueued_at: Mutex<VecDeque<Instant>>,
    dropped_audio_chunks: AtomicU64,
}

enum QueueCommand {
    Work {
        item: TranscriptionWorkItem,
        configuration_revision: u64,
    },
    Chunk {
        audio: AudioChunk,
        enqueued_at: MonotonicTimestamp,
        configuration_revision: u64,
    },
    Finish(oneshot::Sender<()>),
}

struct QueueLane {
    sender: mpsc::Sender<QueueCommand>,
    metrics: Arc<LaneMetrics>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TranscriptionQueueMetrics {
    pub queue_depth: usize,
    pub oldest_segment_age_ms: Option<u64>,
    pub newest_segment_age_ms: Option<u64>,
    pub segments_dropped: u64,
}

/// One ordered worker per audio source. A slow microphone provider call can no longer
/// delay system-output audio before it even reaches its own provider session.
pub struct TranscriptionQueue {
    microphone: QueueLane,
    system_output: QueueLane,
    runtime: Arc<TranscriptionRuntime>,
    dropped: Arc<AtomicU64>,
}

impl TranscriptionQueue {
    pub fn spawn(runtime: Arc<TranscriptionRuntime>) -> Self {
        let dropped = Arc::new(AtomicU64::new(0));
        TranscriptionQueue {
            microphone: Self::spawn_lane(AudioSource::Microphone, Arc::clone(&runtime)),
            system_output: Self::spawn_lane(AudioSource::SystemOutput, Arc::clone(&runtime)),
            runtime,
            dropped,
        }
    }

    fn spawn_lane(source: AudioSource, runtime: Arc<TranscriptionRuntime>) -> QueueLane {
        let (sender, mut receiver) = mpsc::channel::<QueueCommand>(QUEUE_CAPACITY);
        let metrics = Arc::new(LaneMetrics::default());
        let worker_metrics = Arc::clone(&metrics);

        tauri::async_runtime::spawn(async move {
            while let Some(command) = receiver.recv().await {
                match command {
                    QueueCommand::Work {
                        item,
                        configuration_revision,
                    } => {
                        let queued_age_ms = item.enqueued_at.elapsed_ms();
                        worker_metrics
                            .enqueued_at
                            .lock()
                            .expect("transcription queue metrics mutex poisoned")
                            .pop_front();
                        if queued_age_ms >= 1_000 {
                            debug!(
                                ?source,
                                queued_age_ms, "transcription queue processing old audio"
                            );
                        }
                        if let Err(error) = runtime
                            .push_work_item_for_revision(item, configuration_revision)
                            .await
                        {
                            debug!(?source, %error, "transcription of segment failed");
                        }
                    }
                    QueueCommand::Chunk {
                        audio,
                        enqueued_at,
                        configuration_revision,
                    } => {
                        let queued_age_ms = enqueued_at.elapsed_ms();
                        let (queue_depth, oldest_age_ms) = {
                            let mut queued = worker_metrics
                                .enqueued_at
                                .lock()
                                .expect("transcription queue metrics mutex poisoned");
                            queued.pop_front();
                            let now = Instant::now();
                            (
                                queued.len(),
                                queued.front().map(|at| {
                                    now.saturating_duration_since(*at).as_millis() as u64
                                }),
                            )
                        };
                        if queued_age_ms >= 1_000 {
                            debug!(
                                ?source,
                                queued_age_ms, "transcription queue processing old audio"
                            );
                        }
                        runtime.record_streaming_queue_metrics(
                            source,
                            if matches!(
                                audio.activity,
                                crate::transcription::session::AudioActivity::Start
                            ) {
                                audio.activity_observed_at
                            } else {
                                None
                            },
                            queued_age_ms,
                            queue_depth,
                            oldest_age_ms,
                            worker_metrics.dropped_audio_chunks.load(Ordering::Relaxed),
                        );
                        if let Err(error) = runtime
                            .push_chunk_for_revision(audio, configuration_revision)
                            .await
                        {
                            debug!(?source, %error, "streaming transcription chunk failed");
                        }
                    }
                    QueueCommand::Finish(acknowledge) => {
                        runtime.finish_source(source).await;
                        let _ = acknowledge.send(());
                    }
                }
            }
        });

        QueueLane { sender, metrics }
    }

    fn lane(&self, source: AudioSource) -> &QueueLane {
        match source {
            AudioSource::Microphone => &self.microphone,
            AudioSource::SystemOutput => &self.system_output,
        }
    }

    pub fn accepts_continuous_audio(&self) -> bool {
        self.runtime.provider_capabilities().streaming
    }

    pub fn streaming_audio_config(
        &self,
    ) -> Option<crate::transcription::session::StreamingAudioConfig> {
        self.runtime.streaming_audio_config()
    }

    pub async fn prepare_source(
        &self,
        source: AudioSource,
        capture_stream_id: crate::audio::types::CaptureStreamId,
    ) -> Result<(), crate::transcription::error::TranscriptionError> {
        self.runtime.prepare_source(source, capture_stream_id).await
    }

    /// Boundary markers cannot be dropped: they share the source lane with audio and use
    /// backpressure only on the forwarding task, never on the capture callback.
    pub async fn enqueue_ordered_chunk(&self, audio: AudioChunk) {
        if self.runtime.active_session_id().is_none() {
            return;
        }
        let source = audio.source;
        let enqueued_at = MonotonicTimestamp::now();
        let configuration_revision = self.runtime.configuration_revision();
        let lane = self.lane(source);
        lane.metrics
            .enqueued_at
            .lock()
            .expect("transcription queue metrics mutex poisoned")
            .push_back(enqueued_at.as_instant());
        if lane
            .sender
            .send(QueueCommand::Chunk {
                audio,
                enqueued_at,
                configuration_revision,
            })
            .await
            .is_err()
        {
            let mut timestamps = lane
                .metrics
                .enqueued_at
                .lock()
                .expect("transcription queue metrics mutex poisoned");
            if let Some(position) = timestamps
                .iter()
                .position(|timestamp| *timestamp == enqueued_at.as_instant())
            {
                timestamps.remove(position);
            }
        }
    }

    /// Continuous providers receive capture frames directly instead of waiting for the
    /// local VAD to finalize a segment. The same bounded/drop-newest policy still applies.
    pub fn try_enqueue_chunk(&self, audio: AudioChunk) {
        if self.runtime.active_session_id().is_none() {
            return;
        }
        let source = audio.source;
        let enqueued_at = MonotonicTimestamp::now();
        let configuration_revision = self.runtime.configuration_revision();
        let lane = self.lane(source);
        let mut timestamps = lane
            .metrics
            .enqueued_at
            .lock()
            .expect("transcription queue metrics mutex poisoned");
        if lane
            .sender
            .try_send(QueueCommand::Chunk {
                audio,
                enqueued_at,
                configuration_revision,
            })
            .is_err()
        {
            lane.metrics
                .dropped_audio_chunks
                .fetch_add(1, Ordering::Relaxed);
            let dropped = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            if dropped % 50 == 1 {
                warn!(
                    ?source,
                    dropped_audio_items = dropped,
                    "transcription queue full, dropping streaming audio"
                );
            }
        } else {
            timestamps.push_back(enqueued_at.as_instant());
        }
    }

    /// Graceful close is ordered after every accepted work item in this source lane.
    /// This prevents a slow queued segment from reopening a provider session after capture
    /// has already reported itself stopped.
    pub async fn finish_source(&self, source: AudioSource) {
        let (acknowledge, finished) = oneshot::channel();
        if self
            .lane(source)
            .sender
            .send(QueueCommand::Finish(acknowledge))
            .await
            .is_err()
        {
            warn!(
                ?source,
                "transcription queue worker unavailable during source finish"
            );
            return;
        }
        let _ = finished.await;
    }

    /// Never blocks. A full queue drops the newest segment and records the loss.
    pub fn try_enqueue(&self, segment: AudioSegment) {
        let enqueued_at = MonotonicTimestamp::now();
        let Some(session_id) = self.runtime.active_session_id() else {
            debug!(
                source = ?segment.source,
                "segment discarded: no active conversation session"
            );
            return;
        };

        if let Err(error) = SourceIntegrityError::check(
            segment.id,
            segment.source,
            segment.source,
            IntegrityStage::Enqueue,
        ) {
            crate::integrity::origin_log().record_violation(error);
            return;
        }

        let source = segment.source;
        let configuration_revision = self.runtime.configuration_revision();
        let item =
            TranscriptionWorkItem::from_segment(session_id, segment, enqueued_at, enqueued_at);
        let lane = self.lane(source);
        let mut timestamps = lane
            .metrics
            .enqueued_at
            .lock()
            .expect("transcription queue metrics mutex poisoned");

        if lane
            .sender
            .try_send(QueueCommand::Work {
                item,
                configuration_revision,
            })
            .is_err()
        {
            let dropped = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            if dropped % 50 == 1 {
                warn!(
                    ?source,
                    dropped_segments = dropped,
                    "transcription queue full, dropping segment"
                );
            }
        } else {
            timestamps.push_back(enqueued_at.as_instant());
        }
    }

    pub fn dropped_segments(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    pub fn metrics(&self) -> TranscriptionQueueMetrics {
        let now = Instant::now();
        let microphone = self
            .microphone
            .metrics
            .enqueued_at
            .lock()
            .expect("transcription queue metrics mutex poisoned");
        let system_output = self
            .system_output
            .metrics
            .enqueued_at
            .lock()
            .expect("transcription queue metrics mutex poisoned");
        let age = |at: &Instant| {
            now.saturating_duration_since(*at)
                .as_millis()
                .min(u128::from(u64::MAX)) as u64
        };

        let oldest = microphone
            .front()
            .into_iter()
            .chain(system_output.front())
            .min()
            .map(age);
        let newest = microphone
            .back()
            .into_iter()
            .chain(system_output.back())
            .max()
            .map(age);

        TranscriptionQueueMetrics {
            queue_depth: microphone.len() + system_output.len(),
            oldest_segment_age_ms: oldest,
            newest_segment_age_ms: newest,
            segments_dropped: self.dropped_segments(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::segment::AudioTimestamp;
    use crate::audio::types::AudioSource;
    use crate::conversation::SessionId;
    use crate::transcription::fake_provider::{FakeBehavior, FakeTranscriptionProvider};
    use crate::transcription::runtime::{TranscriptionOutputSink, TranscriptionRuntimeOutput};
    use crate::transcription::settings::TranscriptionSettings;
    use std::sync::Mutex as StdMutex;
    use tokio::sync::Notify;

    fn segment() -> AudioSegment {
        AudioSegment::new(
            AudioSource::SystemOutput,
            vec![0.0; 1_600],
            16_000,
            AudioTimestamp(0),
            AudioTimestamp(100),
        )
    }

    fn runtime_with(
        behavior: FakeBehavior,
        sink: TranscriptionOutputSink,
    ) -> Arc<TranscriptionRuntime> {
        Arc::new(TranscriptionRuntime::new(
            Arc::new(FakeTranscriptionProvider::new(behavior)),
            TranscriptionSettings::default(),
            sink,
        ))
    }

    #[tokio::test]
    async fn a_queued_segment_reaches_the_runtime_and_produces_a_final() {
        let notify = Arc::new(Notify::new());
        let outputs: Arc<StdMutex<Vec<TranscriptionRuntimeOutput>>> =
            Arc::new(StdMutex::new(Vec::new()));
        let outputs_cb = Arc::clone(&outputs);
        let notify_cb = Arc::clone(&notify);
        let sink: TranscriptionOutputSink = Arc::new(move |output| {
            let is_final = matches!(output, TranscriptionRuntimeOutput::Final(_));
            outputs_cb.lock().unwrap().push(output);
            if is_final {
                notify_cb.notify_one();
            }
        });

        let runtime = runtime_with(
            FakeBehavior::EmitsFinal {
                text: "olá".into(),
                partials: false,
            },
            sink,
        );
        runtime.begin_session(SessionId::from_value(1)).await;
        let queue = TranscriptionQueue::spawn(Arc::clone(&runtime));

        queue.try_enqueue(segment());
        notify.notified().await;

        let finals = outputs
            .lock()
            .unwrap()
            .iter()
            .filter(|o| matches!(o, TranscriptionRuntimeOutput::Final(_)))
            .count();
        assert_eq!(finals, 1);
    }

    #[tokio::test]
    async fn a_failing_segment_does_not_stop_the_worker() {
        let notify = Arc::new(Notify::new());
        let notify_cb = Arc::clone(&notify);
        let errors = Arc::new(StdMutex::new(0usize));
        let errors_cb = Arc::clone(&errors);
        let sink: TranscriptionOutputSink = Arc::new(move |output| {
            if let TranscriptionRuntimeOutput::Event(
                crate::transcription::events::TranscriptionEvent::Error(_),
            ) = output
            {
                *errors_cb.lock().unwrap() += 1;
                notify_cb.notify_one();
            }
        });

        let runtime = runtime_with(
            FakeBehavior::Fails {
                message: "falha simulada".into(),
            },
            sink,
        );
        runtime.begin_session(SessionId::from_value(1)).await;
        let queue = TranscriptionQueue::spawn(Arc::clone(&runtime));

        queue.try_enqueue(segment());
        notify.notified().await;
        queue.try_enqueue(segment());
        notify.notified().await;

        assert_eq!(*errors.lock().unwrap(), 2);
    }

    #[tokio::test]
    async fn finish_is_ordered_after_already_accepted_audio() {
        let outputs: Arc<StdMutex<Vec<TranscriptionRuntimeOutput>>> =
            Arc::new(StdMutex::new(Vec::new()));
        let sink_outputs = Arc::clone(&outputs);
        let sink: TranscriptionOutputSink =
            Arc::new(move |output| sink_outputs.lock().unwrap().push(output));
        let runtime = runtime_with(
            FakeBehavior::EmitsFinalAfter {
                text: "último segmento".into(),
                delay: std::time::Duration::from_millis(20),
            },
            sink,
        );
        runtime.begin_session(SessionId::from_value(1)).await;
        let queue = TranscriptionQueue::spawn(Arc::clone(&runtime));

        queue.try_enqueue(segment());
        queue.finish_source(AudioSource::SystemOutput).await;

        assert_eq!(
            outputs
                .lock()
                .unwrap()
                .iter()
                .filter(|output| matches!(output, TranscriptionRuntimeOutput::Final(_)))
                .count(),
            1
        );
        assert!(runtime.active_transcription_sessions().is_empty());
    }

    #[tokio::test]
    async fn full_queue_drops_and_counts_instead_of_blocking() {
        let sink: TranscriptionOutputSink = Arc::new(|_| {});
        let runtime = runtime_with(
            FakeBehavior::EmitsFinalAfter {
                text: "lento".into(),
                delay: std::time::Duration::from_secs(60),
            },
            sink,
        );
        runtime.begin_session(SessionId::from_value(1)).await;
        let queue = TranscriptionQueue::spawn(runtime);

        // First segment is picked up immediately by the worker (leaving the channel
        // empty), so fill the channel itself past capacity with the rest.
        for _ in 0..(QUEUE_CAPACITY + 5) {
            queue.try_enqueue(segment());
        }

        assert!(queue.dropped_segments() > 0);
        let metrics = queue.metrics();
        assert!(metrics.queue_depth > 0);
        assert!(metrics.queue_depth <= QUEUE_CAPACITY);
        assert!(metrics.oldest_segment_age_ms.is_some());
        assert!(metrics.newest_segment_age_ms.is_some());
        assert_eq!(metrics.segments_dropped, queue.dropped_segments());
    }
}
