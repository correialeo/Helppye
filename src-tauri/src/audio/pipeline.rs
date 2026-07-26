use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::audio::config::CaptureConfig;
use crate::audio::error::AudioCaptureError;
use crate::audio::provider::AudioCaptureProvider;
use crate::audio::resampler::{downmix_to_mono, resample_linear};
use crate::audio::types::{AudioCaptureEvent, AudioDevice, AudioFrame, AudioSource};

/// Microphone capture via `cpal`, cross-platform (Windows/macOS/Linux).
///
/// The native audio callback never blocks: it downmixes/accumulates samples under a
/// short-lived mutex and pushes finished frames with `try_send`. If the consumer is
/// lagging and the bounded channel is full, the frame is dropped and counted — the
/// callback never waits and the channel never grows unbounded.
pub struct MicrophoneCaptureProvider;

#[async_trait]
impl AudioCaptureProvider for MicrophoneCaptureProvider {
    async fn list_devices(&self) -> Result<Vec<AudioDevice>, AudioCaptureError> {
        crate::audio::devices::list_input_devices()
    }

    async fn start(
        &self,
        config: CaptureConfig,
        sender: mpsc::Sender<AudioCaptureEvent>,
        cancel: CancellationToken,
    ) -> Result<(), AudioCaptureError> {
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<Result<(), AudioCaptureError>>();

        std::thread::Builder::new()
            .name("helppye-mic-capture".into())
            .spawn(move || run_capture_thread(config, sender, cancel, ready_tx))
            .map_err(|e| {
                AudioCaptureError::Internal(format!("failed to spawn capture thread: {e}"))
            })?;

        ready_rx.await.map_err(|_| {
            AudioCaptureError::Internal("capture thread exited before signaling readiness".into())
        })?
    }
}

fn run_capture_thread(
    config: CaptureConfig,
    sender: mpsc::Sender<AudioCaptureEvent>,
    cancel: CancellationToken,
    ready_tx: tokio::sync::oneshot::Sender<Result<(), AudioCaptureError>>,
) {
    let host = cpal::default_host();

    let device = match &config.device_id {
        Some(id) => host
            .input_devices()
            .ok()
            .and_then(|mut devs| devs.find(|d| d.name().map(|n| &n == id).unwrap_or(false))),
        None => None,
    }
    .or_else(|| host.default_input_device());

    let Some(device) = device else {
        let _ = ready_tx.send(Err(AudioCaptureError::NoDeviceFound));
        return;
    };

    let device_name = device.name().unwrap_or_else(|_| "unknown".to_string());

    let stream_config = match device.default_input_config() {
        Ok(c) if c.sample_format() == cpal::SampleFormat::F32 => c,
        Ok(_) => {
            let _ = ready_tx.send(Err(AudioCaptureError::StreamBuildFailed(
                "device default format is not F32 (unsupported in this MVP)".into(),
            )));
            return;
        }
        Err(e) => {
            let _ = ready_tx.send(Err(AudioCaptureError::DeviceUnavailable(e.to_string())));
            return;
        }
    };

    let native_rate = stream_config.sample_rate().0;
    let native_channels = stream_config.channels();
    let target_rate = config.target_sample_rate;
    let frame_len = config.frame_len_samples();

    let start = Instant::now();
    let dropped_frames = Arc::new(AtomicU64::new(0));
    let dropped_frames_cb = dropped_frames.clone();
    let accumulator: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::with_capacity(frame_len * 2)));
    let accumulator_cb = accumulator.clone();
    let tx = sender.clone();

    let err_tx = sender.clone();
    let stream = device.build_input_stream(
        &stream_config.config(),
        move |data: &[f32], _| {
            let mono_native = downmix_to_mono(data, native_channels);
            let mono_target = resample_linear(&mono_native, native_rate, target_rate);

            let mut buf = match accumulator_cb.lock() {
                Ok(b) => b,
                Err(_) => return, // poisoned accumulator: drop this callback's data rather than panic in a real-time callback
            };
            buf.extend_from_slice(&mono_target);

            while buf.len() >= frame_len {
                let samples: Vec<f32> = buf.drain(..frame_len).collect();
                let frame = AudioFrame {
                    source: AudioSource::Microphone,
                    samples,
                    sample_rate: target_rate,
                    channels: 1,
                    timestamp_ms: start.elapsed().as_millis() as u64,
                };
                if tx.try_send(AudioCaptureEvent::Frame(frame)).is_err() {
                    let n = dropped_frames_cb.fetch_add(1, Ordering::Relaxed) + 1;
                    if n % 50 == 1 {
                        warn!(
                            dropped_frames = n,
                            "mic capture consumer lagging, dropping frames"
                        );
                    }
                }
            }
        },
        move |err| {
            error!(%err, "cpal input stream error");
            let _ = err_tx.try_send(AudioCaptureEvent::Error {
                source: AudioSource::Microphone,
                message: err.to_string(),
            });
        },
        None,
    );

    let stream = match stream {
        Ok(s) => s,
        Err(e) => {
            let _ = ready_tx.send(Err(AudioCaptureError::StreamBuildFailed(e.to_string())));
            return;
        }
    };

    if let Err(e) = stream.play() {
        let _ = ready_tx.send(Err(AudioCaptureError::StreamBuildFailed(e.to_string())));
        return;
    }

    info!(device = %device_name, native_rate, native_channels, target_rate, "microphone capture started");
    let _ = ready_tx.send(Ok(()));
    let _ = sender.try_send(AudioCaptureEvent::Started {
        device: AudioDevice {
            id: device_name.clone(),
            name: device_name,
            source: AudioSource::Microphone,
            is_default: config.device_id.is_none(),
        },
    });

    while !cancel.is_cancelled() {
        std::thread::sleep(Duration::from_millis(50));
    }

    drop(stream);
    debug!(
        dropped_frames = dropped_frames.load(Ordering::Relaxed),
        "microphone capture stopped"
    );
    let _ = sender.try_send(AudioCaptureEvent::Stopped {
        source: AudioSource::Microphone,
    });
}
