//! Bounded queue decoupling segment production (segmentation, running inline on the
//! capture event-forwarding task) from transcription (potentially slow model inference).
//! The producer side always uses `try_send`, so a backed-up queue drops the newest
//! segment and counts it instead of ever blocking capture or the UI thread — the same
//! drop-and-log-every-50 policy already used for audio frames (`audio::pipeline`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::warn;

use crate::audio::segment::AudioSegment;
use crate::transcription::provider::TranscriptionProvider;
use crate::transcription::types::TranscriptEvent;

/// Small on purpose: segments already represent multiple seconds of speech each, so a deep
/// backlog here means transcription has fallen far behind real time, not a transient blip.
pub const QUEUE_CAPACITY: usize = 16;

/// Owns the single worker task that drains queued segments through one
/// `TranscriptionProvider`, one at a time, and reports every outcome — success or
/// failure — via `on_event`. Never panics on a failed segment; never lets a slow segment
/// stop the next one from being queued.
pub struct TranscriptionQueue {
    sender: mpsc::Sender<AudioSegment>,
    dropped: AtomicU64,
}

impl TranscriptionQueue {
    pub fn spawn(
        provider: Arc<dyn TranscriptionProvider>,
        on_event: impl Fn(TranscriptEvent) + Send + Sync + 'static,
    ) -> Self {
        let (sender, mut receiver) = mpsc::channel::<AudioSegment>(QUEUE_CAPACITY);

        tokio::spawn(async move {
            while let Some(segment) = receiver.recv().await {
                let segment_id = segment.id;
                let source = segment.source;
                let event = match provider.transcribe(segment).await {
                    Ok(transcript) => TranscriptEvent::Ready(transcript),
                    Err(e) => TranscriptEvent::Failed {
                        segment_id,
                        source,
                        message: e.to_string(),
                    },
                };
                on_event(event);
            }
        });

        TranscriptionQueue {
            sender,
            dropped: AtomicU64::new(0),
        }
    }

    /// Never blocks. A full queue drops `segment` and counts it, logging every 50th drop
    /// rather than every one.
    pub fn try_enqueue(&self, segment: AudioSegment) {
        if self.sender.try_send(segment).is_err() {
            let n = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            if n % 50 == 1 {
                warn!(
                    dropped_segments = n,
                    "transcription queue full, dropping segment"
                );
            }
        }
    }

    pub fn dropped_segments(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::segment::AudioTimestamp;
    use crate::audio::types::AudioSource;
    use crate::transcription::error::TranscriptionError;
    use crate::transcription::types::{ModelConfig, Transcript};
    use async_trait::async_trait;
    use std::sync::Mutex as StdMutex;
    use tokio::sync::Notify;

    struct FakeProvider {
        delay: Option<std::time::Duration>,
        fail: bool,
    }

    #[async_trait]
    impl TranscriptionProvider for FakeProvider {
        async fn load(&self, _config: ModelConfig) -> Result<(), TranscriptionError> {
            Ok(())
        }

        async fn transcribe(
            &self,
            segment: AudioSegment,
        ) -> Result<Transcript, TranscriptionError> {
            if let Some(delay) = self.delay {
                tokio::time::sleep(delay).await;
            }
            if self.fail {
                return Err(TranscriptionError::InferenceFailed("fake failure".into()));
            }
            Ok(Transcript {
                segment_id: segment.id,
                source: segment.source,
                text: "fake transcript".into(),
                language: Some("pt".into()),
                started_at: segment.started_at,
                ended_at: segment.ended_at,
                processing_time_ms: 0,
            })
        }

        fn provider_name(&self) -> &'static str {
            "fake"
        }
    }

    fn segment() -> AudioSegment {
        AudioSegment::new(
            AudioSource::SystemOutput,
            vec![0.0; 1_600],
            16_000,
            AudioTimestamp(0),
            AudioTimestamp(100),
        )
    }

    #[tokio::test]
    async fn successful_transcription_reaches_the_event_callback() {
        let notify = Arc::new(Notify::new());
        let results: Arc<StdMutex<Vec<TranscriptEvent>>> = Arc::new(StdMutex::new(Vec::new()));
        let results_cb = results.clone();
        let notify_cb = notify.clone();

        let queue = TranscriptionQueue::spawn(
            Arc::new(FakeProvider {
                delay: None,
                fail: false,
            }),
            move |event| {
                results_cb.lock().unwrap().push(event);
                notify_cb.notify_one();
            },
        );

        queue.try_enqueue(segment());
        notify.notified().await;

        let results = results.lock().unwrap();
        assert_eq!(results.len(), 1);
        assert!(matches!(results[0], TranscriptEvent::Ready(_)));
    }

    #[tokio::test]
    async fn failed_transcription_reaches_the_event_callback_as_failed() {
        let notify = Arc::new(Notify::new());
        let results: Arc<StdMutex<Vec<TranscriptEvent>>> = Arc::new(StdMutex::new(Vec::new()));
        let results_cb = results.clone();
        let notify_cb = notify.clone();

        let queue = TranscriptionQueue::spawn(
            Arc::new(FakeProvider {
                delay: None,
                fail: true,
            }),
            move |event| {
                results_cb.lock().unwrap().push(event);
                notify_cb.notify_one();
            },
        );

        queue.try_enqueue(segment());
        notify.notified().await;

        let results = results.lock().unwrap();
        assert_eq!(results.len(), 1);
        assert!(matches!(results[0], TranscriptEvent::Failed { .. }));
    }

    #[tokio::test]
    async fn full_queue_drops_and_counts_instead_of_blocking() {
        let queue = TranscriptionQueue::spawn(
            Arc::new(FakeProvider {
                delay: Some(std::time::Duration::from_secs(60)),
                fail: false,
            }),
            |_event| {},
        );

        // First segment is picked up immediately by the worker (leaving the channel
        // empty), so fill the channel itself past capacity with the rest.
        for _ in 0..(QUEUE_CAPACITY + 5) {
            queue.try_enqueue(segment());
        }

        assert!(queue.dropped_segments() > 0);
    }
}
