use cpal::traits::{DeviceTrait, HostTrait};

use crate::audio::error::AudioCaptureError;
use crate::audio::types::{AudioDevice, AudioSource};

/// Lists microphone (input) devices via `cpal`'s default host. This is intentionally
/// cross-platform and simple — device *selection* by name is fragile (see the Meetily
/// audit, `docs/meetily-audio-audit.md` section 6, finding 7); callers should treat
/// `AudioDevice::id` as best-effort and fall back to the default device when a lookup fails.
pub fn list_input_devices() -> Result<Vec<AudioDevice>, AudioCaptureError> {
    let host = cpal::default_host();
    let default_name = host.default_input_device().and_then(|d| d.name().ok());

    let devices = host
        .input_devices()
        .map_err(|e| AudioCaptureError::DeviceUnavailable(e.to_string()))?;

    let mut result = Vec::new();
    for device in devices {
        let Ok(name) = device.name() else { continue };
        let is_default = default_name.as_deref() == Some(name.as_str());
        result.push(AudioDevice {
            id: name.clone(),
            name,
            source: AudioSource::Microphone,
            is_default,
        });
    }
    Ok(result)
}
