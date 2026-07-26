#[derive(Debug, Clone)]
pub struct CaptureConfig {
    /// `None` selects the platform default device.
    pub device_id: Option<String>,
    pub target_sample_rate: u32,
    /// The pipeline only emits mono frames; this is always 1 downstream of resampling.
    pub target_channels: u16,
    pub frame_duration_ms: u32,
    /// Capacity of the bounded event channel handed to `AudioCaptureProvider::start`.
    pub channel_capacity: usize,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            device_id: None,
            target_sample_rate: 16_000,
            target_channels: 1,
            frame_duration_ms: 100,
            channel_capacity: 32,
        }
    }
}

impl CaptureConfig {
    pub fn frame_len_samples(&self) -> usize {
        ((self.target_sample_rate as u64 * self.frame_duration_ms as u64) / 1000) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_targets_16khz_mono_100ms_frames() {
        let config = CaptureConfig::default();
        assert_eq!(config.target_sample_rate, 16_000);
        assert_eq!(config.target_channels, 1);
        assert_eq!(config.frame_len_samples(), 1_600);
    }

    #[test]
    fn frame_len_scales_with_duration_and_rate() {
        let config = CaptureConfig {
            target_sample_rate: 48_000,
            frame_duration_ms: 20,
            ..CaptureConfig::default()
        };
        assert_eq!(config.frame_len_samples(), 960);
    }
}
