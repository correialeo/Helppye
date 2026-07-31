//! Bounded queue decoupling segment production (segmentation, running inline on the
//! capture event-forwarding task) from transcription (potentially slow model inference).
//! The producer side always uses `try_send`, so a backed-up queue drops the newest
//! segment and counts it instead of ever blocking capture or the UI thread — the same
//! drop-and-log-every-50 policy already used for audio frames (`audio::pipeline`).
//!
//! O worker entrega cada segmento ao `TranscriptionRuntime`, não a um transcritor direto:
//! é o runtime que sabe a qual sessão de transcrição aquela fonte pertence e é ele quem
//! publica os resultados. A fila continua sendo só o amortecedor entre produção e consumo.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::audio::segment::AudioSegment;
use crate::transcription::runtime::TranscriptionRuntime;

/// Small on purpose: segments already represent multiple seconds of speech each, so a deep
/// backlog here means transcription has fallen far behind real time, not a transient blip.
pub const QUEUE_CAPACITY: usize = 16;

/// Owns the single worker task that drains queued segments into the runtime, one at a
/// time. Never panics on a failed segment; never lets a slow segment stop the next one from
/// being queued.
pub struct TranscriptionQueue {
    sender: mpsc::Sender<AudioSegment>,
    dropped: AtomicU64,
    runtime: Arc<TranscriptionRuntime>,
}

impl TranscriptionQueue {
    pub fn spawn(runtime: Arc<TranscriptionRuntime>) -> Self {
        let (sender, mut receiver) = mpsc::channel::<AudioSegment>(QUEUE_CAPACITY);
        let worker_runtime = Arc::clone(&runtime);

        // `tokio::spawn` panics here: `spawn` runs synchronously inside Tauri's
        // `.setup()` hook, outside any Tokio task context. `tauri::async_runtime::spawn`
        // uses Tauri's own managed runtime instead (same reason `audio::start_capture`
        // uses it for its forwarding task).
        tauri::async_runtime::spawn(async move {
            while let Some(segment) = receiver.recv().await {
                let source = segment.source;
                if let Err(e) = worker_runtime.push_segment(segment).await {
                    // Falha de um segmento não derruba o worker: o próximo segmento pode
                    // transcrever normalmente, e o runtime já publicou o evento de erro
                    // correspondente para quem observa.
                    debug!(?source, %e, "transcription of a segment failed");
                }
            }
        });

        TranscriptionQueue {
            sender,
            dropped: AtomicU64::new(0),
            runtime,
        }
    }

    /// Encerramento gracioso da sessão de transcrição de uma fonte, quando a captura dela
    /// para sem que a sessão de conversa acabe. A fila fica aberta: a outra fonte continua
    /// entregando normalmente.
    pub async fn finish_source(&self, source: crate::audio::types::AudioSource) {
        self.runtime.finish_source(source).await;
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
    }
}
