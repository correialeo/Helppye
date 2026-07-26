//! Output (render) device enumeration and resolution via the WASAPI `IMMDeviceEnumerator`.
//!
//! Devices are identified by their WASAPI endpoint ID (`IMMDevice::GetId`), a stable string
//! that survives reconnection/renaming — unlike the name-substring matching the Meetily
//! audit flagged as fragile (`docs/meetily-audio-audit.md` section 6, finding 7).

use tracing::warn;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::{
    eConsole, eRender, IMMDevice, IMMDeviceCollection, IMMDeviceEnumerator, MMDeviceEnumerator,
    DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToStringAlloc};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL, STGM_READ};

use crate::audio::error::AudioCaptureError;
use crate::audio::types::{AudioDevice, AudioSource};

use super::com::ComGuard;

/// Lists active render (output) devices — the candidates for loopback capture.
pub fn list_output_devices() -> Result<Vec<AudioDevice>, AudioCaptureError> {
    let _com = ComGuard::new()?;
    let enumerator = create_enumerator()?;

    // SAFETY: `enumerator` is a valid, just-created IMMDeviceEnumerator.
    let default_id = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }
        .ok()
        .and_then(|d| unsafe { device_id(&d) }.ok());

    // SAFETY: `enumerator` is valid; eRender/DEVICE_STATE_ACTIVE are well-formed argument values.
    let collection: IMMDeviceCollection =
        unsafe { enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) }
            .map_err(|e| AudioCaptureError::Internal(format!("EnumAudioEndpoints failed: {e}")))?;

    // SAFETY: `collection` is a valid, just-populated IMMDeviceCollection.
    let count =
        unsafe { collection.GetCount() }.map_err(|e| AudioCaptureError::Internal(e.to_string()))?;

    let mut result = Vec::with_capacity(count as usize);
    for i in 0..count {
        // SAFETY: `i` is in `0..count`, the range `GetCount` just reported as valid.
        let device = match unsafe { collection.Item(i) } {
            Ok(d) => d,
            Err(e) => {
                warn!(%e, index = i, "failed to get output device at index, skipping");
                continue;
            }
        };
        let id = match unsafe { device_id(&device) } {
            Ok(id) => id,
            Err(e) => {
                warn!(%e, "failed to read output device id, skipping");
                continue;
            }
        };
        let name = match unsafe { friendly_name(&device) } {
            Ok(name) => name,
            Err(e) => {
                warn!(%e, id, "failed to read output device friendly name, using id");
                id.clone()
            }
        };
        let is_default = default_id.as_deref() == Some(id.as_str());
        result.push(AudioDevice {
            id,
            name,
            source: AudioSource::SystemOutput,
            is_default,
        });
    }
    Ok(result)
}

/// Resolves the `IMMDevice` to open for loopback capture: by stable endpoint ID when given
/// (falling back to the default render device if the ID is no longer present), else the
/// default render device outright.
pub fn resolve_output_device(
    enumerator: &IMMDeviceEnumerator,
    device_id_filter: Option<&str>,
) -> Result<IMMDevice, AudioCaptureError> {
    if let Some(id) = device_id_filter {
        let wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: `enumerator` is valid; `wide` is a null-terminated UTF-16 buffer kept
        // alive for the duration of this call.
        let by_id = unsafe { enumerator.GetDevice(PCWSTR(wide.as_ptr())) };
        if let Ok(device) = by_id {
            return Ok(device);
        }
        warn!(
            id,
            "configured output device not found, falling back to default"
        );
    }
    // SAFETY: `enumerator` is valid; eRender/eConsole are well-formed argument values.
    unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }
        .map_err(|e| AudioCaptureError::DeviceUnavailable(format!("no default output device: {e}")))
}

pub(super) fn create_enumerator() -> Result<IMMDeviceEnumerator, AudioCaptureError> {
    // SAFETY: CoCreateInstance with the well-known MMDeviceEnumerator CLSID and the
    // IMMDeviceEnumerator IID inferred from the return type; the returned COM pointer is
    // owned by the returned wrapper and released on its `Drop`.
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.map_err(|e| {
        AudioCaptureError::Internal(format!("CoCreateInstance(MMDeviceEnumerator) failed: {e}"))
    })
}

/// # Safety
/// `device` must be a valid `IMMDevice`.
pub unsafe fn device_id(device: &IMMDevice) -> Result<String, AudioCaptureError> {
    let pwstr = device
        .GetId()
        .map_err(|e| AudioCaptureError::Internal(format!("IMMDevice::GetId failed: {e}")))?;
    let s = pwstr_to_string(pwstr);
    // SAFETY: `pwstr` was allocated by `GetId` via CoTaskMemAlloc; it is freed exactly once,
    // here, after copying its contents into an owned `String` above.
    CoTaskMemFree(Some(pwstr.0 as *const core::ffi::c_void));
    Ok(s)
}

/// # Safety
/// `device` must be a valid `IMMDevice`.
pub unsafe fn friendly_name(device: &IMMDevice) -> Result<String, AudioCaptureError> {
    let store = device
        .OpenPropertyStore(STGM_READ)
        .map_err(|e| AudioCaptureError::Internal(format!("OpenPropertyStore failed: {e}")))?;
    let mut prop = store.GetValue(&PKEY_Device_FriendlyName).map_err(|e| {
        AudioCaptureError::Internal(format!("GetValue(PKEY_Device_FriendlyName) failed: {e}"))
    })?;
    // SAFETY: `prop` was just populated by `GetValue` above; `PropVariantToStringAlloc`
    // is the Microsoft-provided safe accessor for extracting a string from a PROPVARIANT
    // regardless of its exact VT_* string subtype, avoiding manual union access.
    let name_result = PropVariantToStringAlloc(&prop);
    // SAFETY: releases any resources owned by `prop` (e.g. the string it holds); must run
    // exactly once, after the value has been read above.
    let _ = PropVariantClear(&mut prop);
    let name_pwstr = name_result.map_err(|e| {
        AudioCaptureError::Internal(format!("PropVariantToStringAlloc failed: {e}"))
    })?;
    let name = pwstr_to_string(name_pwstr);
    // SAFETY: `name_pwstr` was allocated by `PropVariantToStringAlloc` via CoTaskMemAlloc.
    CoTaskMemFree(Some(name_pwstr.0 as *const core::ffi::c_void));
    Ok(name)
}

/// # Safety
/// `pwstr` must be a valid, null-terminated UTF-16 string pointer (or null).
unsafe fn pwstr_to_string(pwstr: PWSTR) -> String {
    if pwstr.0.is_null() {
        return String::new();
    }
    pwstr.to_string().unwrap_or_default()
}
