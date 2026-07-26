//! Hardware-independent PCM sample conversion.
//!
//! Pure functions over byte buffers so they can be unit-tested on any platform (they back
//! the WASAPI loopback path on Windows, see `audio::platform::windows::format`, but don't
//! depend on it). WASAPI's shared-mode mix format is essentially always 32-bit IEEE float
//! (`IAudioClient::GetMixFormat` returns the audio engine's internal format, which has been
//! float since Windows Vista), so the integer PCM paths here are a defensive fallback for
//! unusual drivers rather than the common case.

/// Describes how one sample occupies its container in an interleaved PCM buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleContainer {
    F32,
    /// Signed integer PCM. `container_bytes` is the container size (2, 3, or 4), taken
    /// from the format's bits-per-sample. For containers wider than the true bit depth
    /// (e.g. 24-bit audio in a 32-bit container), this assumes the sample is
    /// left-justified — i.e. the raw container value can be treated as a full-range
    /// integer of `container_bytes` width. This is the common WASAPI convention, but
    /// unusual drivers that right-justify without re-scaling would produce quieter
    /// output than expected; there's no way to detect that from the format alone.
    IntPcm {
        container_bytes: u8,
    },
}

impl SampleContainer {
    pub fn byte_size(self) -> usize {
        match self {
            SampleContainer::F32 => 4,
            SampleContainer::IntPcm { container_bytes } => container_bytes as usize,
        }
    }
}

/// Converts one sample's raw little-endian bytes (length must equal `container.byte_size()`)
/// to `f32` in `[-1.0, 1.0]`.
pub fn convert_frame_to_f32(bytes: &[u8], container: SampleContainer) -> f32 {
    debug_assert_eq!(bytes.len(), container.byte_size());
    match container {
        SampleContainer::F32 => {
            let arr: [u8; 4] = bytes.try_into().expect("F32 container is always 4 bytes");
            f32::from_le_bytes(arr)
        }
        SampleContainer::IntPcm { container_bytes } => {
            let n = container_bytes as usize;
            let mut buf = [0u8; 4];
            buf[..n].copy_from_slice(&bytes[..n]);
            if n < 4 && bytes[n - 1] & 0x80 != 0 {
                for b in &mut buf[n..] {
                    *b = 0xFF;
                }
            }
            let raw = i32::from_le_bytes(buf);
            let max = 2f64.powi(container_bytes as i32 * 8 - 1);
            (raw as f64 / max) as f32
        }
    }
}

/// Converts an interleaved raw PCM buffer to interleaved `f32`. `data.len()` must be a
/// multiple of `container.byte_size()` (guaranteed by WASAPI's block-aligned buffers).
pub fn convert_buffer_to_f32(data: &[u8], container: SampleContainer) -> Vec<f32> {
    let stride = container.byte_size();
    data.chunks_exact(stride)
        .map(|chunk| convert_frame_to_f32(chunk, container))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f32_container_passes_through_bit_exact() {
        let bytes = 0.5f32.to_le_bytes();
        assert_eq!(convert_frame_to_f32(&bytes, SampleContainer::F32), 0.5);
    }

    #[test]
    fn i16_min_and_max_map_to_full_scale() {
        let container = SampleContainer::IntPcm { container_bytes: 2 };
        assert_eq!(
            convert_frame_to_f32(&(-32768i16).to_le_bytes(), container),
            -1.0
        );
        let max = convert_frame_to_f32(&32767i16.to_le_bytes(), container);
        assert!((max - 1.0).abs() < 0.001);
    }

    #[test]
    fn i24_packed_half_scale_value() {
        let container = SampleContainer::IntPcm { container_bytes: 3 };
        // 0x400000 = 4_194_304 = half of 2^23 (8_388_608)
        let bytes = [0x00, 0x00, 0x40];
        let out = convert_frame_to_f32(&bytes, container);
        assert!((out - 0.5).abs() < 1e-6, "expected ~0.5, got {out}");
    }

    #[test]
    fn i24_packed_negative_value_sign_extends() {
        let container = SampleContainer::IntPcm { container_bytes: 3 };
        // 0xC00000 as 24-bit signed = -4_194_304 = -0.5 * 2^23
        let bytes = [0x00, 0x00, 0xC0];
        let out = convert_frame_to_f32(&bytes, container);
        assert!((out - -0.5).abs() < 1e-6, "expected ~-0.5, got {out}");
    }

    #[test]
    fn i32_min_and_max_map_to_full_scale() {
        let container = SampleContainer::IntPcm { container_bytes: 4 };
        assert_eq!(
            convert_frame_to_f32(&i32::MIN.to_le_bytes(), container),
            -1.0
        );
        let max = convert_frame_to_f32(&i32::MAX.to_le_bytes(), container);
        assert!((max - 1.0).abs() < 0.001);
    }

    #[test]
    fn buffer_conversion_splits_on_container_stride() {
        let container = SampleContainer::IntPcm { container_bytes: 2 };
        let mut data = Vec::new();
        data.extend_from_slice(&0i16.to_le_bytes());
        data.extend_from_slice(&(-32768i16).to_le_bytes());
        data.extend_from_slice(&32767i16.to_le_bytes());
        let out = convert_buffer_to_f32(&data, container);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0], 0.0);
        assert_eq!(out[1], -1.0);
    }
}
