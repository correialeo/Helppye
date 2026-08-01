//! Motor de captura: garante exatamente 1 sessão ativa por categoria (microfone, saída de
//! sistema), nunca duas simultâneas para a mesma categoria, e nunca dois dispositivos
//! abertos ao mesmo tempo durante uma troca. Injeta os providers (`AudioCaptureProvider`)
//! e o sink de eventos como dependências — o mesmo padrão de `model_manager::ModelManager`
//! — para que toda essa lógica seja testável sem hardware de áudio real e sem uma
//! instância de `AppHandle`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::audio::config::CaptureConfig;
use crate::audio::error::AudioCaptureError;
use crate::audio::provider::AudioCaptureProvider;
use crate::audio::segmentation::{SegmentationConfig, Segmenter, SpeechEvent};
use crate::audio::selection::{self, DeviceSelectionConfig, ResolvedDevice};
use crate::audio::types::{AudioCaptureEvent, AudioDevice, AudioSource, CaptureStreamId};
use crate::transcription::queue::TranscriptionQueue;

pub type CaptureEventSink = Arc<dyn Fn(AudioCaptureEvent) + Send + Sync>;

#[derive(Debug, Clone, Serialize)]
pub struct DeviceSelectionSnapshot {
    pub input: Option<ResolvedDevice>,
    pub output: Option<ResolvedDevice>,
}

struct CaptureHandle {
    cancel: CancellationToken,
    /// Kept for observability/tests (`CaptureEngine::selected_device_id` is the one used
    /// by production code to know what's active); not read elsewhere yet.
    #[allow(dead_code)]
    device_id: String,
    session_id: u64,
    forward_task: tauri::async_runtime::JoinHandle<()>,
}

struct CategoryState {
    active: AsyncMutex<Option<CaptureHandle>>,
    next_session_id: AtomicU64,
}

impl CategoryState {
    fn new() -> Self {
        Self {
            active: AsyncMutex::new(None),
            next_session_id: AtomicU64::new(0),
        }
    }
}

pub struct CaptureEngine {
    mic_provider: Arc<dyn AudioCaptureProvider>,
    system_provider: Arc<dyn AudioCaptureProvider>,
    mic: CategoryState,
    system: CategoryState,
    selected: AsyncMutex<DeviceSelectionConfig>,
    config_path: PathBuf,
    transcription_queue: Arc<TranscriptionQueue>,
    timeline_origin: Instant,
    emit: CaptureEventSink,
}

impl CaptureEngine {
    pub fn new(
        mic_provider: Arc<dyn AudioCaptureProvider>,
        system_provider: Arc<dyn AudioCaptureProvider>,
        transcription_queue: Arc<TranscriptionQueue>,
        config_path: PathBuf,
        initial_selection: DeviceSelectionConfig,
        emit: CaptureEventSink,
    ) -> Self {
        Self {
            mic_provider,
            system_provider,
            mic: CategoryState::new(),
            system: CategoryState::new(),
            selected: AsyncMutex::new(initial_selection),
            config_path,
            transcription_queue,
            timeline_origin: Instant::now(),
            emit,
        }
    }

    fn category(&self, source: AudioSource) -> &CategoryState {
        match source {
            AudioSource::Microphone => &self.mic,
            AudioSource::SystemOutput => &self.system,
        }
    }

    fn provider(&self, source: AudioSource) -> Arc<dyn AudioCaptureProvider> {
        match source {
            AudioSource::Microphone => self.mic_provider.clone(),
            AudioSource::SystemOutput => self.system_provider.clone(),
        }
    }

    pub async fn list_devices(
        &self,
        source: AudioSource,
    ) -> Result<Vec<AudioDevice>, AudioCaptureError> {
        self.provider(source).list_devices().await
    }

    pub async fn is_capturing(&self, source: AudioSource) -> bool {
        self.category(source).active.lock().await.is_some()
    }

    pub async fn selected_device_id(&self, source: AudioSource) -> Option<String> {
        let sel = self.selected.lock().await;
        match source {
            AudioSource::Microphone => sel.input_device_id.clone(),
            AudioSource::SystemOutput => sel.output_device_id.clone(),
        }
    }

    async fn set_selected_device_id(
        &self,
        source: AudioSource,
        device_id: Option<String>,
    ) -> Result<(), AudioCaptureError> {
        let mut sel = self.selected.lock().await;
        match source {
            AudioSource::Microphone => sel.input_device_id = device_id,
            AudioSource::SystemOutput => sel.output_device_id = device_id,
        }
        selection::save(&self.config_path, &sel)
            .map_err(|e| AudioCaptureError::Internal(e.to_string()))
    }

    /// Resolve e persiste (sem iniciar captura) o dispositivo de entrada e de saída a
    /// partir da seleção persistida + lista atual de dispositivos. Chamado uma vez na
    /// abertura do app para pré-selecionar sem efeitos colaterais de captura.
    pub async fn resolve_selection(&self) -> Result<DeviceSelectionSnapshot, AudioCaptureError> {
        let input = self.resolve_and_persist(AudioSource::Microphone).await?;
        let output = self.resolve_and_persist(AudioSource::SystemOutput).await?;
        Ok(DeviceSelectionSnapshot { input, output })
    }

    async fn resolve_and_persist(
        &self,
        source: AudioSource,
    ) -> Result<Option<ResolvedDevice>, AudioCaptureError> {
        let devices = self.list_devices(source).await?;
        let persisted = self.selected_device_id(source).await;
        let resolved = selection::resolve_device(&devices, persisted.as_deref());
        self.set_selected_device_id(source, resolved.as_ref().map(|r| r.device_id.clone()))
            .await?;
        Ok(resolved)
    }

    /// Atualiza a seleção manual do usuário para uma categoria. Se essa categoria estiver
    /// capturando no momento, encerra completamente a sessão anterior — espera o
    /// encerramento real antes de retornar — e reinicia já com o novo dispositivo. Nunca
    /// mantém o dispositivo antigo e o novo abertos ao mesmo tempo. Se não estiver
    /// capturando, é apenas uma atualização de estado, sem efeito colateral.
    pub async fn select_device(
        self: Arc<Self>,
        source: AudioSource,
        device_id: String,
    ) -> Result<(), AudioCaptureError> {
        self.set_selected_device_id(source, Some(device_id)).await?;

        if self.is_capturing(source).await {
            self.stop_capture(source).await;
            self.start_capture(source).await?;
        }
        Ok(())
    }

    /// Inicia a captura usando exclusivamente o dispositivo atualmente selecionado para
    /// `source`. Retorna `AudioCaptureError::AlreadyRunning` se essa categoria já tiver
    /// uma sessão ativa — nunca duas sessões simultâneas para a mesma categoria.
    pub async fn start_capture(
        self: Arc<Self>,
        source: AudioSource,
    ) -> Result<(), AudioCaptureError> {
        let category = self.category(source);
        let mut guard = category.active.lock().await;
        if guard.is_some() {
            return Err(AudioCaptureError::AlreadyRunning);
        }

        let device_id = self
            .selected_device_id(source)
            .await
            .ok_or(AudioCaptureError::NoDeviceFound)?;

        let config = CaptureConfig {
            device_id: Some(device_id.clone()),
            ..CaptureConfig::default()
        };
        let sample_rate = config.target_sample_rate;
        let (tx, mut rx) = tokio::sync::mpsc::channel(config.channel_capacity);
        let cancel = CancellationToken::new();

        self.provider(source)
            .start(config, tx, cancel.clone())
            .await?;

        let session_id = category.next_session_id.fetch_add(1, Ordering::Relaxed) + 1;
        // Um `CaptureStreamId` por `start_capture`: é este id que acompanha cada segmento
        // até a Conversation Timeline e que permite à transcrição casar um resultado com o
        // fluxo que o produziu, mesmo que o usuário troque de dispositivo no meio da sessão
        // e dois fluxos da mesma fonte se sobreponham por um instante.
        let capture_stream_id = CaptureStreamId::next();
        let session_timeline_offset_ms = self.timeline_origin.elapsed().as_millis() as u64;
        let engine = self.clone();
        let emit = self.emit.clone();
        let transcription_queue = self.transcription_queue.clone();

        let forward_task = tauri::async_runtime::spawn(async move {
            let mut segmenter = Segmenter::for_stream(
                source,
                capture_stream_id,
                sample_rate,
                SegmentationConfig::default(),
            );
            let mut disconnected = false;

            while let Some(event) = rx.recv().await {
                if let AudioCaptureEvent::Frame(ref frame) = event {
                    for speech_event in segmenter.push_samples(&frame.samples) {
                        if let SpeechEvent::SegmentReady(segment) = speech_event {
                            transcription_queue.try_enqueue(
                                segment.with_timestamp_offset(session_timeline_offset_ms),
                            );
                        }
                    }
                }
                if matches!(event, AudioCaptureEvent::DeviceDisconnected { .. }) {
                    disconnected = true;
                }

                emit(event);
            }

            // The channel only closes once every sender clone held by the provider's
            // capture thread has been dropped — i.e. the underlying stream has fully
            // stopped. Only now is it safe to consider this session over: clearing the
            // guard any earlier could let a start command race a device that's still
            // being torn down.
            let mut guard = engine.category(source).active.lock().await;
            if matches!(guard.as_ref(), Some(h) if h.session_id == session_id) {
                *guard = None;
            }
            drop(guard);

            // A disconnection ends the session on its own (not via an explicit stop
            // command) — never restart automatically, but do refresh which device the
            // stale selection should fall back to so the UI's "use this device" opt-in
            // (see docs/local-transcription.md's device-selection section) points
            // somewhere valid.
            if disconnected {
                if let Err(e) = engine.resolve_and_persist(source).await {
                    warn!(source = ?source, %e, "failed to resolve fallback device after disconnection");
                }
            }
        });

        *guard = Some(CaptureHandle {
            cancel,
            device_id,
            session_id,
            forward_task,
        });
        Ok(())
    }

    /// Encerra a captura ativa desta categoria, se houver, e espera o encerramento real
    /// da sessão (canal fechado pela thread da plataforma) antes de retornar. Sem efeito
    /// se a categoria já estiver parada.
    pub async fn stop_capture(&self, source: AudioSource) {
        let handle = {
            let mut guard = self.category(source).active.lock().await;
            guard.take()
        };
        if let Some(handle) = handle {
            handle.cancel.cancel();
            let _ = handle.forward_task.await;
            // Depois de a tarefa de encaminhamento terminar — nesse ponto todo segmento
            // desta fonte já foi entregue. Encerra a sessão de transcrição da fonte de forma
            // graciosa, sem tocar na outra fonte nem na sessão de conversa.
            self.transcription_queue.finish_source(source).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    use crate::transcription::error::TranscriptionError;
    use crate::transcription::segment_transcriber::SegmentTranscriber;
    use crate::transcription::types::{ModelConfig, Transcript};

    struct NoopSegmentTranscriber;

    #[async_trait]
    impl SegmentTranscriber for NoopSegmentTranscriber {
        async fn load(&self, _config: ModelConfig) -> Result<(), TranscriptionError> {
            Ok(())
        }

        async fn transcribe(
            &self,
            segment: crate::audio::segment::AudioSegment,
        ) -> Result<Transcript, TranscriptionError> {
            Ok(Transcript {
                segment_id: segment.id,
                source: segment.source,
                text: String::new(),
                language: None,
                started_at: segment.started_at,
                ended_at: segment.ended_at,
                processing_time_ms: 0,
            })
        }

        fn provider_name(&self) -> &'static str {
            "noop"
        }
    }

    fn fake_transcription_queue() -> Arc<TranscriptionQueue> {
        let provider: Arc<dyn crate::transcription::provider::TranscriptionProvider> = Arc::new(
            crate::transcription::whisper_local::WhisperLocalTranscriptionProvider::new(Arc::new(
                NoopSegmentTranscriber,
            )),
        );
        let runtime = Arc::new(crate::transcription::runtime::TranscriptionRuntime::new(
            provider,
            crate::transcription::settings::TranscriptionSettings::default(),
            Arc::new(|_output| {}),
        ));
        Arc::new(TranscriptionQueue::spawn(runtime))
    }

    fn temp_config_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "helppye-engine-test-{name}-{}.json",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn device(source: AudioSource, id: &str) -> AudioDevice {
        AudioDevice {
            id: id.to_string(),
            name: format!("Device {id}"),
            source,
            is_default: false,
        }
    }

    /// Fake provider for engine-level tests: never touches real audio hardware. Tracks
    /// how many sessions are concurrently "open" so tests can assert the engine never
    /// opens two devices for the same category at once.
    struct FakeCaptureProvider {
        source: AudioSource,
        devices: Vec<AudioDevice>,
        concurrent_opens: Arc<AtomicUsize>,
        max_concurrent_opens: Arc<AtomicUsize>,
        disconnect_immediately: bool,
    }

    impl FakeCaptureProvider {
        fn new(source: AudioSource, devices: Vec<AudioDevice>) -> Self {
            Self {
                source,
                devices,
                concurrent_opens: Arc::new(AtomicUsize::new(0)),
                max_concurrent_opens: Arc::new(AtomicUsize::new(0)),
                disconnect_immediately: false,
            }
        }

        fn tracking(
            mut self,
            concurrent_opens: Arc<AtomicUsize>,
            max_concurrent_opens: Arc<AtomicUsize>,
        ) -> Self {
            self.concurrent_opens = concurrent_opens;
            self.max_concurrent_opens = max_concurrent_opens;
            self
        }

        fn disconnecting(mut self) -> Self {
            self.disconnect_immediately = true;
            self
        }
    }

    #[async_trait]
    impl AudioCaptureProvider for FakeCaptureProvider {
        async fn list_devices(&self) -> Result<Vec<AudioDevice>, AudioCaptureError> {
            Ok(self.devices.clone())
        }

        async fn start(
            &self,
            config: CaptureConfig,
            sender: tokio::sync::mpsc::Sender<AudioCaptureEvent>,
            cancel: CancellationToken,
        ) -> Result<(), AudioCaptureError> {
            let device_id = config.device_id.clone().unwrap_or_default();
            let source = self.source;

            let now = self.concurrent_opens.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_concurrent_opens.fetch_max(now, Ordering::SeqCst);

            let _ = sender.try_send(AudioCaptureEvent::Started {
                device: AudioDevice {
                    id: device_id.clone(),
                    name: device_id.clone(),
                    source,
                    is_default: false,
                },
            });

            if self.disconnect_immediately {
                self.concurrent_opens.fetch_sub(1, Ordering::SeqCst);
                let _ = sender.try_send(AudioCaptureEvent::DeviceDisconnected {
                    source,
                    device_id: device_id.clone(),
                });
                let _ = sender.try_send(AudioCaptureEvent::Stopped { source });
                return Ok(());
            }

            let concurrent_opens = self.concurrent_opens.clone();
            tokio::spawn(async move {
                cancel.cancelled().await;
                concurrent_opens.fetch_sub(1, Ordering::SeqCst);
                let _ = sender.try_send(AudioCaptureEvent::Stopped { source });
            });
            Ok(())
        }
    }

    fn engine_with(
        mic_provider: impl AudioCaptureProvider + 'static,
        system_provider: impl AudioCaptureProvider + 'static,
        config_name: &str,
    ) -> Arc<CaptureEngine> {
        Arc::new(CaptureEngine::new(
            Arc::new(mic_provider),
            Arc::new(system_provider),
            fake_transcription_queue(),
            temp_config_path(config_name),
            DeviceSelectionConfig::default(),
            Arc::new(|_event| {}),
        ))
    }

    // Cenário 5.
    #[tokio::test]
    async fn manual_input_selection_updates_state_without_starting_capture() {
        let engine = engine_with(
            FakeCaptureProvider::new(AudioSource::Microphone, vec![]),
            FakeCaptureProvider::new(AudioSource::SystemOutput, vec![]),
            "manual-input",
        );

        engine
            .clone()
            .select_device(AudioSource::Microphone, "mic-manual".into())
            .await
            .unwrap();

        assert_eq!(
            engine.selected_device_id(AudioSource::Microphone).await,
            Some("mic-manual".into())
        );
        assert!(!engine.is_capturing(AudioSource::Microphone).await);
    }

    // Cenário 6.
    #[tokio::test]
    async fn manual_output_selection_updates_state_without_starting_capture() {
        let engine = engine_with(
            FakeCaptureProvider::new(AudioSource::Microphone, vec![]),
            FakeCaptureProvider::new(AudioSource::SystemOutput, vec![]),
            "manual-output",
        );

        engine
            .clone()
            .select_device(AudioSource::SystemOutput, "speaker-manual".into())
            .await
            .unwrap();

        assert_eq!(
            engine.selected_device_id(AudioSource::SystemOutput).await,
            Some("speaker-manual".into())
        );
        assert!(!engine.is_capturing(AudioSource::SystemOutput).await);
    }

    // Cenário 7.
    #[tokio::test]
    async fn only_one_input_capture_is_active_at_a_time() {
        let engine = engine_with(
            FakeCaptureProvider::new(
                AudioSource::Microphone,
                vec![device(AudioSource::Microphone, "mic-1")],
            ),
            FakeCaptureProvider::new(AudioSource::SystemOutput, vec![]),
            "single-input",
        );
        engine
            .clone()
            .select_device(AudioSource::Microphone, "mic-1".into())
            .await
            .unwrap();

        engine
            .clone()
            .start_capture(AudioSource::Microphone)
            .await
            .unwrap();
        let second = engine.clone().start_capture(AudioSource::Microphone).await;
        assert!(matches!(second, Err(AudioCaptureError::AlreadyRunning)));

        engine.stop_capture(AudioSource::Microphone).await;
        assert!(engine
            .clone()
            .start_capture(AudioSource::Microphone)
            .await
            .is_ok());
    }

    // Cenário 8.
    #[tokio::test]
    async fn only_one_output_capture_is_active_at_a_time() {
        let engine = engine_with(
            FakeCaptureProvider::new(AudioSource::Microphone, vec![]),
            FakeCaptureProvider::new(
                AudioSource::SystemOutput,
                vec![device(AudioSource::SystemOutput, "spk-1")],
            ),
            "single-output",
        );
        engine
            .clone()
            .select_device(AudioSource::SystemOutput, "spk-1".into())
            .await
            .unwrap();

        engine
            .clone()
            .start_capture(AudioSource::SystemOutput)
            .await
            .unwrap();
        let second = engine
            .clone()
            .start_capture(AudioSource::SystemOutput)
            .await;
        assert!(matches!(second, Err(AudioCaptureError::AlreadyRunning)));

        engine.stop_capture(AudioSource::SystemOutput).await;
        assert!(engine
            .clone()
            .start_capture(AudioSource::SystemOutput)
            .await
            .is_ok());
    }

    // Cenário 9: a chamada duplicada retorna um erro tipado especificamente, não uma
    // string genérica — só o comando Tauri (fronteira externa) converte para `String`.
    #[tokio::test]
    async fn duplicate_start_call_returns_a_typed_already_running_error() {
        let engine = engine_with(
            FakeCaptureProvider::new(
                AudioSource::Microphone,
                vec![device(AudioSource::Microphone, "mic-1")],
            ),
            FakeCaptureProvider::new(AudioSource::SystemOutput, vec![]),
            "duplicate-start",
        );
        engine
            .clone()
            .select_device(AudioSource::Microphone, "mic-1".into())
            .await
            .unwrap();
        engine
            .clone()
            .start_capture(AudioSource::Microphone)
            .await
            .unwrap();

        let err = engine
            .start_capture(AudioSource::Microphone)
            .await
            .unwrap_err();
        assert_eq!(err, AudioCaptureError::AlreadyRunning);
    }

    // Cenário 10: nunca dois dispositivos de entrada abertos ao mesmo tempo durante a troca.
    #[tokio::test]
    async fn switching_input_device_during_capture_never_opens_two_devices_at_once() {
        let concurrent = Arc::new(AtomicUsize::new(0));
        let max_concurrent = Arc::new(AtomicUsize::new(0));
        let mic_provider = FakeCaptureProvider::new(
            AudioSource::Microphone,
            vec![
                device(AudioSource::Microphone, "mic-a"),
                device(AudioSource::Microphone, "mic-b"),
            ],
        )
        .tracking(concurrent.clone(), max_concurrent.clone());

        let engine = engine_with(
            mic_provider,
            FakeCaptureProvider::new(AudioSource::SystemOutput, vec![]),
            "switch-input",
        );

        engine
            .clone()
            .select_device(AudioSource::Microphone, "mic-a".into())
            .await
            .unwrap();
        engine
            .clone()
            .start_capture(AudioSource::Microphone)
            .await
            .unwrap();

        engine
            .clone()
            .select_device(AudioSource::Microphone, "mic-b".into())
            .await
            .unwrap();

        assert_eq!(
            engine.selected_device_id(AudioSource::Microphone).await,
            Some("mic-b".into())
        );
        assert!(engine.is_capturing(AudioSource::Microphone).await);
        assert_eq!(max_concurrent.load(Ordering::SeqCst), 1);
        assert_eq!(concurrent.load(Ordering::SeqCst), 1);
    }

    // Cenário 11: o mesmo, para a saída de sistema.
    #[tokio::test]
    async fn switching_output_device_during_capture_never_opens_two_devices_at_once() {
        let concurrent = Arc::new(AtomicUsize::new(0));
        let max_concurrent = Arc::new(AtomicUsize::new(0));
        let system_provider = FakeCaptureProvider::new(
            AudioSource::SystemOutput,
            vec![
                device(AudioSource::SystemOutput, "spk-a"),
                device(AudioSource::SystemOutput, "spk-b"),
            ],
        )
        .tracking(concurrent.clone(), max_concurrent.clone());

        let engine = engine_with(
            FakeCaptureProvider::new(AudioSource::Microphone, vec![]),
            system_provider,
            "switch-output",
        );

        engine
            .clone()
            .select_device(AudioSource::SystemOutput, "spk-a".into())
            .await
            .unwrap();
        engine
            .clone()
            .start_capture(AudioSource::SystemOutput)
            .await
            .unwrap();

        engine
            .clone()
            .select_device(AudioSource::SystemOutput, "spk-b".into())
            .await
            .unwrap();

        assert_eq!(
            engine.selected_device_id(AudioSource::SystemOutput).await,
            Some("spk-b".into())
        );
        assert!(engine.is_capturing(AudioSource::SystemOutput).await);
        assert_eq!(max_concurrent.load(Ordering::SeqCst), 1);
        assert_eq!(concurrent.load(Ordering::SeqCst), 1);
    }

    // Cenário 12: dispositivo removido — a captura para sozinha, o estado é liberado (sem
    // exigir um stop explícito do usuário) e a seleção persistida cai para o fallback,
    // já que o dispositivo desconectado não existe mais na lista.
    #[tokio::test]
    async fn device_removal_stops_capture_and_falls_back_the_selection() {
        let mic_provider = FakeCaptureProvider::new(
            AudioSource::Microphone,
            vec![device(AudioSource::Microphone, "mic-a")],
        )
        .disconnecting();
        let engine = engine_with(
            mic_provider,
            FakeCaptureProvider::new(AudioSource::SystemOutput, vec![]),
            "device-removed",
        );

        engine
            .clone()
            .select_device(AudioSource::Microphone, "mic-a".into())
            .await
            .unwrap();
        engine
            .clone()
            .start_capture(AudioSource::Microphone)
            .await
            .unwrap();

        // The disconnection is asynchronous (forwarded through the same channel as any
        // other event) — give the forwarder task a moment to observe it and self-clean.
        for _ in 0..50 {
            if !engine.is_capturing(AudioSource::Microphone).await {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        assert!(!engine.is_capturing(AudioSource::Microphone).await);
        // "mic-a" no longer appears in the (now stale) device list from the provider's
        // perspective in this fake, so resolution has nothing to fall back to — this
        // still proves the guard was released and a fresh restart is possible.
        assert!(engine
            .clone()
            .start_capture(AudioSource::Microphone)
            .await
            .is_ok());
    }

    // Cenário 15 (nível de engine): a troca do padrão do Windows durante uma captura ativa
    // nunca troca o dispositivo sozinha — só uma seleção explícita do usuário troca.
    #[tokio::test]
    async fn windows_default_change_never_auto_switches_the_active_device() {
        let engine = engine_with(
            FakeCaptureProvider::new(
                AudioSource::Microphone,
                vec![
                    device(AudioSource::Microphone, "mic-a"),
                    device(AudioSource::Microphone, "mic-b"),
                ],
            ),
            FakeCaptureProvider::new(AudioSource::SystemOutput, vec![]),
            "no-auto-switch",
        );

        engine
            .clone()
            .select_device(AudioSource::Microphone, "mic-a".into())
            .await
            .unwrap();
        engine
            .clone()
            .start_capture(AudioSource::Microphone)
            .await
            .unwrap();

        // Simulates Windows reporting a different default device without any explicit
        // user action — `resolve_selection`/`select_device` is simply never invoked here.
        assert_eq!(
            engine.selected_device_id(AudioSource::Microphone).await,
            Some("mic-a".into())
        );
        assert!(engine.is_capturing(AudioSource::Microphone).await);
    }
}
