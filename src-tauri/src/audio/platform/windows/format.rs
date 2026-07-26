//! Parses WASAPI `WAVEFORMATEX`/`WAVEFORMATEXTENSIBLE` into a container-agnostic description
//! that `audio::sample_convert` can act on without depending on Windows types.

use windows::Win32::Media::Audio::{WAVEFORMATEX, WAVEFORMATEXTENSIBLE, WAVE_FORMAT_PCM};
use windows::Win32::Media::KernelStreaming::{KSDATAFORMAT_SUBTYPE_PCM, WAVE_FORMAT_EXTENSIBLE};
use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};

use crate::audio::error::AudioCaptureError;
use crate::audio::sample_convert::SampleContainer;

pub struct WaveFormat {
    pub channels: u16,
    pub sample_rate: u32,
    pub container: SampleContainer,
}

/// Parses a format pointer as returned by `IAudioClient::GetMixFormat`/
/// `GetClosestMatchFormat` into a `WaveFormat`.
///
/// # Safety
/// `ptr` must point to a valid, fully initialized `WAVEFORMATEX` for the duration of this
/// call. If `wFormatTag == WAVE_FORMAT_EXTENSIBLE` and `cbSize >= 22`, the allocation
/// backing `ptr` must be at least `size_of::<WAVEFORMATEXTENSIBLE>()` bytes — both
/// conditions are guaranteed by WASAPI for anything returned from `GetMixFormat`.
pub unsafe fn parse_wave_format(ptr: *const WAVEFORMATEX) -> Result<WaveFormat, AudioCaptureError> {
    if ptr.is_null() {
        return Err(AudioCaptureError::Internal(
            "WASAPI returned a null format pointer".into(),
        ));
    }
    let base = &*ptr;
    let channels = base.nChannels;
    let sample_rate = base.nSamplesPerSec;
    let container_bytes = (base.wBitsPerSample / 8) as u8;
    if container_bytes == 0 {
        return Err(AudioCaptureError::Internal(
            "WASAPI format reported 0 bits per sample".into(),
        ));
    }

    let is_extensible = base.wFormatTag as u32 == WAVE_FORMAT_EXTENSIBLE && base.cbSize >= 22;

    let is_float = if is_extensible {
        // SAFETY: guaranteed by this function's safety contract when `is_extensible`.
        let ext = &*(ptr as *const WAVEFORMATEXTENSIBLE);
        // WAVEFORMATEXTENSIBLE is a packed struct, so `ext.SubFormat` must be read into an
        // owned value (not referenced in place) before comparing — `GUID` is `Copy`, so
        // this block-expression copy is the standard way to avoid an unaligned reference.
        let sub_format = { ext.SubFormat };
        if sub_format == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT {
            true
        } else if sub_format == KSDATAFORMAT_SUBTYPE_PCM {
            false
        } else {
            return Err(AudioCaptureError::Unsupported(format!(
                "unsupported WASAPI extensible subformat {sub_format:?}"
            )));
        }
    } else {
        match base.wFormatTag as u32 {
            t if t == WAVE_FORMAT_IEEE_FLOAT => true,
            t if t == WAVE_FORMAT_PCM => false,
            t => {
                return Err(AudioCaptureError::Unsupported(format!(
                    "unsupported WASAPI format tag {t}"
                )))
            }
        }
    };

    let container = if is_float {
        SampleContainer::F32
    } else {
        SampleContainer::IntPcm { container_bytes }
    };

    Ok(WaveFormat {
        channels,
        sample_rate,
        container,
    })
}
