/// RMS level of a mono sample buffer, in dBFS (full scale = 0.0, silence approaches -inf).
/// Clamped at -120.0 dB so the UI never has to special-case negative infinity.
pub fn rms_dbfs(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return -120.0;
    }
    let sum_sq: f64 = samples.iter().map(|s| (*s as f64) * (*s as f64)).sum();
    let rms = (sum_sq / samples.len() as f64).sqrt();
    if rms <= 0.0 {
        return -120.0;
    }
    (20.0 * rms.log10()).max(-120.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_is_floor() {
        assert_eq!(rms_dbfs(&[]), -120.0);
        assert_eq!(rms_dbfs(&[0.0, 0.0, 0.0]), -120.0);
    }

    #[test]
    fn full_scale_sine_like_signal_is_near_zero() {
        let samples = vec![1.0, -1.0, 1.0, -1.0];
        let level = rms_dbfs(&samples);
        assert!(
            level > -1.0 && level <= 0.0,
            "expected near 0 dBFS, got {level}"
        );
    }

    #[test]
    fn quieter_signal_has_lower_level_than_louder_signal() {
        let quiet = rms_dbfs(&[0.01, -0.01, 0.01, -0.01]);
        let loud = rms_dbfs(&[0.5, -0.5, 0.5, -0.5]);
        assert!(quiet < loud);
    }
}
