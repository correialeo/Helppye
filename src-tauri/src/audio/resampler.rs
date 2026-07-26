/// Downmixes interleaved multi-channel samples to mono by averaging channels.
pub fn downmix_to_mono(interleaved: &[f32], channels: u16) -> Vec<f32> {
    let channels = channels.max(1) as usize;
    if channels == 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
        .collect()
}

/// Linear-interpolation resampler for mono `f32` samples.
///
/// Quality is intentionally basic (no anti-aliasing filter): it exists to unblock
/// device-rate-independent capture and level metering now. A sinc-based resampler
/// (`rubato`, as identified in the Meetily audit) should replace this before the
/// output feeds a transcription model, where resampling artifacts affect accuracy.
pub fn resample_linear(input: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if from_rate == to_rate || input.is_empty() {
        return input.to_vec();
    }
    let ratio = to_rate as f64 / from_rate as f64;
    let out_len = ((input.len() as f64) * ratio).round() as usize;
    let mut output = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src_pos = i as f64 / ratio;
        let idx = src_pos.floor() as usize;
        let frac = (src_pos - idx as f64) as f32;
        let a = input[idx.min(input.len() - 1)];
        let b = input[(idx + 1).min(input.len() - 1)];
        output.push(a + (b - a) * frac);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmix_stereo_averages_channels() {
        let stereo = vec![1.0, -1.0, 0.5, 0.5];
        assert_eq!(downmix_to_mono(&stereo, 2), vec![0.0, 0.5]);
    }

    #[test]
    fn downmix_mono_is_identity() {
        let mono = vec![0.1, 0.2, 0.3];
        assert_eq!(downmix_to_mono(&mono, 1), mono);
    }

    #[test]
    fn resample_same_rate_is_identity() {
        let input = vec![0.1, 0.2, 0.3];
        assert_eq!(resample_linear(&input, 16_000, 16_000), input);
    }

    #[test]
    fn downsample_halves_length_by_ratio() {
        let input: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let out = resample_linear(&input, 48_000, 16_000);
        // ratio is 1/3, so length should shrink roughly proportionally
        let expected_len = (100.0_f64 * (16_000.0 / 48_000.0)).round() as usize;
        assert_eq!(out.len(), expected_len);
    }

    #[test]
    fn upsample_increases_length() {
        let input = vec![0.0, 1.0, 0.0, 1.0];
        let out = resample_linear(&input, 16_000, 48_000);
        assert_eq!(out.len(), 12);
    }
}
