//! Bounded queue decoupling segment production (segmentation, running inline on the
//! capture event-forwarding task) from transcription (potentially slow model inference).
//! The producer side always uses `try_send`, so a backed-up queue drops the newest
//! segment and counts it instead of ever blocking capture or the UI thread — the same
//! drop-and-log-every-50 policy already used for audio frames (`audio::pipeline`).
//!
//! O worker entrega cada segmento ao `TranscriptionRuntime`, não a um transcritor direto:
//! é o runtime que sabe a qual sessão de transcrição aquela fonte pertence e é ele quem
//! publica os resultados. A fila continua sendo só o amortecedor entre produção e consumo.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Serialize;

use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::audio::segment::AudioSegment;
use crate::integrity::{IntegrityStage, SourceIntegrityError};
use crate::transcription::envelope::{MonotonicTimestamp, TranscriptionWorkItem};
use crate::transcription::runtime::TranscriptionRuntime;

/// Small on purpose: segments already represent multiple seconds of speech each, so a deep
/// backlog here means transcription has fallen far behind real time, not a transient blip.
pub const QUEUE_CAPACITY: usize = 16;

#[derive(Default)]
struct QueueState {
    enqueued_at: Mutex<VecDeque<Instant>>,
    dropped: AtomicU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TranscriptionQueueMetrics {
    pub queue_depth: usize,
    pub oldest_segment_age_ms: Option<u64>,
    pub newest_segment_age_ms: Option<u64>,
    pub segments_dropped: u64,
}

/// Owns the single worker task that drains queued segments into the runtime, one at a
/// time. Never panics on a failed segment; never lets a slow segment stop the next one from
/// being queued.
pub struct TranscriptionQueue {
    sender: mpsc::Sender<TranscriptionWorkItem>,
    state: Arc<QueueState>,
    runtime: Arc<TranscriptionRuntime>,
}

impl TranscriptionQueue {
    pub fn spawn(runtime: Arc<TranscriptionRuntime>) -> Self {
        let (sender, mut receiver) = mpsc::channel::<TranscriptionWorkItem>(QUEUE_CAPACITY);
        let worker_runtime = Arc::clone(&runtime);
        let state = Arc::new(QueueState::default());
        let worker_state = Arc::clone(&state);

        // `tokio::spawn` panics here: `spawn` runs synchronously inside Tauri's
        // `.setup()` hook, outside any Tokio task context. `tauri::async_runtime::spawn`
        // uses Tauri's own managed runtime instead (same reason `audio::start_capture`
        // uses it for its forwarding task).
        tauri::async_runtime::spawn(async move {
            while let Some(item) = receiver.recv().await {
                let queued_age_ms = item.enqueued_at.elapsed_ms();
                worker_state
                    .enqueued_at
                    .lock()
                    .expect("transcription queue metrics mutex poisoned")
                    .pop_front();
                if queued_age_ms >= 1_000 {
                    debug!(queued_age_ms, "transcription queue is processing old audio");
                }
                let source = item.source;
                if let Err(e) = worker_runtime.push_work_item(item).await {
                    // Falha de um segmento não derruba o worker: o próximo segmento pode
                    // transcrever normalmente, e o runtime já publicou o evento de erro
                    // correspondente para quem observa.
                    debug!(?source, %e, "transcription of a segment failed");
                }
            }
        });

        TranscriptionQueue {
            sender,
            state,
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
    ///
    /// É aqui que o segmento vira `TranscriptionWorkItem`: a identidade causal
    /// (`session_id`, `capture_stream_id`, `sequence_number`) é fixada **uma vez**, no ponto
    /// de entrada da camada de transcrição, e todo o resto do pipeline lê dela em vez de
    /// redeclarar a origem. Sem sessão ativa o segmento é descartado aqui mesmo — entrar na
    /// fila para ser recusado adiante só adiaria a mesma decisão gastando uma vaga.
    pub fn try_enqueue(&self, segment: AudioSegment) {
        let enqueued_at = MonotonicTimestamp::now();
        let Some(session_id) = self.runtime.active_session_id() else {
            debug!(
                source = ?segment.source,
                "segmento descartado: nenhuma sessão de conversa ativa"
            );
            return;
        };
        // Comparação, não atribuição: se o segmento chegou aqui já com uma fonte diferente da
        // que o próprio segmento declara ter capturado, o dado é rejeitado em vez de entrar
        // "corrigido". Na prática é sempre `Ok` — o valor está em que deixaria de ser.
        if let Err(error) = SourceIntegrityError::check(
            segment.id,
            segment.source,
            segment.source,
            IntegrityStage::Enqueue,
        ) {
            crate::integrity::origin_log().record_violation(error);
            return;
        }
        let item = TranscriptionWorkItem::from_segment(
            session_id,
            segment,
            // O segmento fica pronto quando a fala termina; entrar na fila é o instante
            // seguinte. Nesta borda os dois coincidem, e separá-los é o que permite medir
            // backlog adiante sem confundi-lo com latência de captura.
            enqueued_at,
            enqueued_at,
        );
        let mut timestamps = self
            .state
            .enqueued_at
            .lock()
            .expect("transcription queue metrics mutex poisoned");
        if self.sender.try_send(item).is_err() {
            let n = self.state.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            if n % 50 == 1 {
                warn!(
                    dropped_segments = n,
                    "transcription queue full, dropping segment"
                );
            }
        } else {
            timestamps.push_back(enqueued_at.as_instant());
        }
    }

    pub fn dropped_segments(&self) -> u64 {
        self.state.dropped.load(Ordering::Relaxed)
    }

    pub fn metrics(&self) -> TranscriptionQueueMetrics {
        let now = Instant::now();
        let timestamps = self
            .state
            .enqueued_at
            .lock()
            .expect("transcription queue metrics mutex poisoned");
        let age = |at: &Instant| {
            now.saturating_duration_since(*at)
                .as_millis()
                .min(u128::from(u64::MAX)) as u64
        };
        TranscriptionQueueMetrics {
            queue_depth: timestamps.len(),
            oldest_segment_age_ms: timestamps.front().map(age),
            newest_segment_age_ms: timestamps.back().map(age),
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
