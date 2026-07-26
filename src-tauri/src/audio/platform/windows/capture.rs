//! WASAPI loopback capture: opens a render (output) device in shared-mode loopback and
//! pulls captured frames on a dedicated thread with no Win32 message pump (see
//! `com::ComGuard` for why MTA is used instead of STA).

use std::time::Instant;

use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    IAudioCaptureClient, IAudioClient, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_E_DEVICE_INVALIDATED,
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
};
use windows::Win32::System::Com::{CoTaskMemFree, CLSCTX_ALL};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use crate::audio::config::CaptureConfig;
use crate::audio::error::AudioCaptureError;
use crate::audio::resampler::{downmix_to_mono, resample_linear};
use crate::audio::sample_convert::convert_buffer_to_f32;
use crate::audio::types::{AudioCaptureEvent, AudioDevice, AudioFrame, AudioSource};

use super::com::ComGuard;
use super::devices::{device_id, friendly_name, resolve_output_device};
use super::format::{parse_wave_format, WaveFormat};

/// How long each `WaitForSingleObject` call blocks before re-checking the cancellation
/// token. WASAPI signals well within this window under normal playback (shared-mode
/// buffer periods are typically ~10ms), so this only bounds shutdown latency.
const WAIT_TIMEOUT_MS: u32 = 100;

/// Handle to a manual-reset-free Win32 event, closed on drop so every early-return path
/// in `setup` cleans up correctly without repeating `CloseHandle` calls.
struct EventHandle(HANDLE);

impl Drop for EventHandle {
    fn drop(&mut self) {
        // SAFETY: `self.0` was created by `CreateEventW` in `setup` and is not shared
        // with any other owner (not duplicated, not passed to another process).
        let _ = unsafe { CloseHandle(self.0) };
    }
}

struct Setup {
    _com: ComGuard,
    audio_client: IAudioClient,
    capture_client: IAudioCaptureClient,
    event: EventHandle,
    wave_format: WaveFormat,
    device: AudioDevice,
}

pub fn run_capture_thread(
    config: CaptureConfig,
    sender: mpsc::Sender<AudioCaptureEvent>,
    cancel: CancellationToken,
    ready_tx: oneshot::Sender<Result<(), AudioCaptureError>>,
) {
    let setup = match setup(&config) {
        Ok(s) => s,
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };

    // SAFETY: `audio_client` was just Initialize()'d successfully in `setup`; Start() is
    // the documented next call in WASAPI's state machine.
    if let Err(e) = unsafe { setup.audio_client.Start() } {
        let _ = ready_tx.send(Err(AudioCaptureError::StreamBuildFailed(format!(
            "IAudioClient::Start failed: {e}"
        ))));
        return;
    }

    info!(
        device = %setup.device.name,
        channels = setup.wave_format.channels,
        sample_rate = setup.wave_format.sample_rate,
        target_rate = config.target_sample_rate,
        "system audio (loopback) capture started"
    );
    let _ = ready_tx.send(Ok(()));
    let _ = sender.try_send(AudioCaptureEvent::Started {
        device: setup.device.clone(),
    });

    run_loop(&config, &setup, &sender, &cancel);

    // SAFETY: `audio_client` is in the Started state reached above; Stop() is always valid
    // to call on a started client and is required before the client is dropped/released.
    if let Err(e) = unsafe { setup.audio_client.Stop() } {
        warn!(%e, "IAudioClient::Stop failed during shutdown");
    }
    let _ = sender.try_send(AudioCaptureEvent::Stopped {
        source: AudioSource::SystemOutput,
    });
}

fn setup(config: &CaptureConfig) -> Result<Setup, AudioCaptureError> {
    let com = ComGuard::new()?;
    let enumerator = super::devices::create_enumerator()?;
    let device = resolve_output_device(&enumerator, config.device_id.as_deref())?;

    // SAFETY: `device` is a valid IMMDevice from `resolve_output_device`. `Activate`
    // creates a new IAudioClient COM object referring to it; `None` for the activation
    // params pointer is valid per the API contract (no activation parameters needed).
    let audio_client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }.map_err(|e| {
        AudioCaptureError::DeviceUnavailable(format!(
            "IMMDevice::Activate(IAudioClient) failed: {e}"
        ))
    })?;

    // SAFETY: `audio_client` was just activated above.
    let format_ptr = unsafe { audio_client.GetMixFormat() }
        .map_err(|e| AudioCaptureError::Internal(format!("GetMixFormat failed: {e}")))?;

    // SAFETY: `format_ptr` was just returned by `GetMixFormat`; per that function's
    // contract it points to a valid WAVEFORMATEX (extended to WAVEFORMATEXTENSIBLE when
    // wFormatTag/cbSize indicate it), non-null on success.
    let parse_result = unsafe { parse_wave_format(format_ptr) };

    // SAFETY: `audio_client` is freshly activated and not yet initialized (its documented
    // required first state transition); `format_ptr` is the same valid pointer used to
    // parse the format above, and `Initialize` only reads from it. `AUDCLNT_STREAMFLAGS_LOOPBACK`
    // opens this *render* device's capture (loopback) client rather than a normal render
    // client — this is the entire mechanism by which system output audio is captured.
    // `AUDCLNT_STREAMFLAGS_EVENTCALLBACK` switches the client into the event-driven model
    // used by `run_loop` below instead of a polling-only model.
    let init_result = unsafe {
        audio_client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
            0,
            0,
            format_ptr,
            None,
        )
    };

    // SAFETY: `format_ptr` was allocated by `GetMixFormat` via CoTaskMemAlloc (WASAPI's
    // documented allocator for this out-pointer); freeing it here is safe because both
    // uses above (`parse_wave_format`, `Initialize`) only read from it and have already
    // returned, and this is the single free for this allocation.
    unsafe { CoTaskMemFree(Some(format_ptr as *const core::ffi::c_void)) };

    let wave_format = parse_result?;
    init_result.map_err(|e| {
        AudioCaptureError::StreamBuildFailed(format!("IAudioClient::Initialize failed: {e}"))
    })?;

    // SAFETY: `pEventHandle` is `None`/null and `bManualReset`/`bInitialState` are both
    // `false`, the standard auto-reset, initially-unsignaled event WASAPI expects for
    // `IAudioClient::SetEventHandle`.
    let event_handle = unsafe { CreateEventW(None, false, false, None) }
        .map_err(|e| AudioCaptureError::Internal(format!("CreateEventW failed: {e}")))?;
    let event = EventHandle(event_handle);

    // SAFETY: `audio_client` was just successfully Initialize()'d with
    // AUDCLNT_STREAMFLAGS_EVENTCALLBACK above (required before calling SetEventHandle);
    // `event.0` is a valid, freshly created event handle owned by `event` for the
    // remainder of this scope.
    unsafe { audio_client.SetEventHandle(event.0) }
        .map_err(|e| AudioCaptureError::Internal(format!("SetEventHandle failed: {e}")))?;

    // SAFETY: `audio_client` is initialized; `GetService` for IAudioCaptureClient is the
    // documented way to obtain the capture interface after Initialize.
    let capture_client: IAudioCaptureClient =
        unsafe { audio_client.GetService() }.map_err(|e| {
            AudioCaptureError::Internal(format!("GetService(IAudioCaptureClient) failed: {e}"))
        })?;

    // SAFETY: `device` is the same valid IMMDevice resolved above.
    let id = unsafe { device_id(&device) }?;
    let name = unsafe { friendly_name(&device) }.unwrap_or_else(|e| {
        warn!(%e, id, "failed to read output device friendly name, using id");
        id.clone()
    });
    let is_default = config.device_id.is_none();

    Ok(Setup {
        _com: com,
        audio_client,
        capture_client,
        event,
        wave_format,
        device: AudioDevice {
            id,
            name,
            source: AudioSource::SystemOutput,
            is_default,
        },
    })
}

fn run_loop(
    config: &CaptureConfig,
    setup: &Setup,
    sender: &mpsc::Sender<AudioCaptureEvent>,
    cancel: &CancellationToken,
) {
    let start = Instant::now();
    let frame_len = config.frame_len_samples();
    let target_rate = config.target_sample_rate;
    let native_rate = setup.wave_format.sample_rate;
    let channels = setup.wave_format.channels;
    let container = setup.wave_format.container;

    let mut accumulator: Vec<f32> = Vec::with_capacity(frame_len * 2);
    let mut dropped_frames: u64 = 0;
    let dropped_log_every = 50;

    while !cancel.is_cancelled() {
        // SAFETY: `setup.event.0` is a valid event handle owned by this thread for the
        // duration of capture; a bounded timeout is used (rather than INFINITE) so this
        // loop can still observe cancellation promptly even if the device stops signaling
        // (e.g. system suspended playback).
        let wait_result = unsafe { WaitForSingleObject(setup.event.0, WAIT_TIMEOUT_MS) };
        if wait_result != WAIT_OBJECT_0 {
            continue; // timeout or (unexpectedly) an error — just re-check cancellation
        }

        loop {
            // SAFETY: `capture_client` is a valid, started IAudioCaptureClient.
            let packet_size = match unsafe { setup.capture_client.GetNextPacketSize() } {
                Ok(n) => n,
                Err(e) => {
                    handle_stream_error(&e, &setup.device.id, sender, cancel);
                    return;
                }
            };
            if packet_size == 0 {
                break;
            }

            let mut data_ptr = std::ptr::null_mut();
            let mut frames_available = 0u32;
            let mut flags = 0u32;
            // SAFETY: all four out-pointers are valid local `&mut` references for the
            // duration of this call, matching `IAudioCaptureClient::GetBuffer`'s contract.
            // `pu64DevicePosition`/`pu64QPCPosition` are not needed and passed as `None`.
            let get_buffer = unsafe {
                setup.capture_client.GetBuffer(
                    &mut data_ptr,
                    &mut frames_available,
                    &mut flags,
                    None,
                    None,
                )
            };
            if let Err(e) = get_buffer {
                handle_stream_error(&e, &setup.device.id, sender, cancel);
                return;
            }

            let is_silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
            let stride = container.byte_size();
            let sample_count = frames_available as usize * channels as usize;

            let interleaved: Vec<f32> = if is_silent {
                // Per WASAPI docs, when AUDCLNT_BUFFERFLAGS_SILENT is set the buffer
                // contents are undefined/should be treated as silence rather than read.
                vec![0.0; sample_count]
            } else {
                // SAFETY: `data_ptr`/`frames_available` describe a buffer WASAPI just
                // handed us via `GetBuffer`, valid for `frames_available * block_align`
                // bytes until the matching `ReleaseBuffer` call below.
                let bytes = unsafe { std::slice::from_raw_parts(data_ptr, sample_count * stride) };
                convert_buffer_to_f32(bytes, container)
            };

            // SAFETY: releases exactly the number of frames reported by this `GetBuffer`
            // call, as required before the next `GetBuffer`/`GetNextPacketSize` call.
            if let Err(e) = unsafe { setup.capture_client.ReleaseBuffer(frames_available) } {
                handle_stream_error(&e, &setup.device.id, sender, cancel);
                return;
            }

            let mono_native = downmix_to_mono(&interleaved, channels);
            let mono_target = resample_linear(&mono_native, native_rate, target_rate);
            accumulator.extend_from_slice(&mono_target);

            while accumulator.len() >= frame_len {
                let samples: Vec<f32> = accumulator.drain(..frame_len).collect();
                let frame = AudioFrame {
                    source: AudioSource::SystemOutput,
                    samples,
                    sample_rate: target_rate,
                    channels: 1,
                    timestamp_ms: start.elapsed().as_millis() as u64,
                };
                if sender.try_send(AudioCaptureEvent::Frame(frame)).is_err() {
                    dropped_frames += 1;
                    if dropped_frames % dropped_log_every == 1 {
                        warn!(
                            dropped_frames,
                            "system audio capture consumer lagging, dropping frames"
                        );
                    }
                }
            }
        }
    }

    debug!(
        dropped_frames,
        "system audio capture loop exiting (cancelled)"
    );
}

/// Classifies a WASAPI call failure during the capture loop: device invalidation (e.g. the
/// output device was unplugged, disabled, or the default device changed) gets a
/// `DeviceDisconnected` event, since Windows offers no clean way to keep capturing across
/// that; anything else is a generic `Error` event. Either way the loop is expected to stop
/// after this call (the caller returns immediately), and `Stopped` is sent by `run_capture_thread`.
fn handle_stream_error(
    err: &windows::core::Error,
    device_id: &str,
    sender: &mpsc::Sender<AudioCaptureEvent>,
    cancel: &CancellationToken,
) {
    if err.code() == AUDCLNT_E_DEVICE_INVALIDATED {
        error!(%err, "system audio device invalidated (disconnected, disabled, or format changed)");
        let _ = sender.try_send(AudioCaptureEvent::DeviceDisconnected {
            source: AudioSource::SystemOutput,
            device_id: device_id.to_string(),
        });
    } else {
        error!(%err, "WASAPI capture call failed");
        let _ = sender.try_send(AudioCaptureEvent::Error {
            source: AudioSource::SystemOutput,
            message: err.to_string(),
        });
    }
    cancel.cancel();
}
