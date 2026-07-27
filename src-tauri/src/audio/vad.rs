//! Lightweight energy-based voice activity detection.
//!
//! Hand-rolled rather than pulling in an external VAD crate: the mature Rust options either
//! wrap WebRTC's C VAD (`webrtc-vad`/`earshot` bindings — a second C toolchain dependency on
//! top of the one the local transcription backend already needs, for a component this small)
//! or are effectively unmaintained. A fixed-window RMS-over-adaptive-threshold detector is a
//! few dozen lines, adds no dependency, reuses `level_meter::rms_dbfs`, and is easy to tune
//! per source (microphone and system-output loopback have very different noise floors).
//! Revisit with a real crate if false positive/negative rates on real recordings prove this
//! too crude — not measurable in this sandbox (no real microphone/loopback hardware here).

use crate::audio::level_meter::rms_dbfs;

/// Initial, non-final tuning — see `docs/speech-segmentation.md` for the rationale and what
/// still needs adjustment against real recordings.
#[derive(Debug, Clone, Copy)]
pub struct VadConfig {
    /// Analysis window size. Independent of the capture pipeline's `CaptureConfig::frame_duration_ms`
    pub window_ms: u32,
    /// dB above the adaptive noise floor a window's RMS must exceed to count as speech.
    pub threshold_margin_db: f32,
    /// EMA smoothing factor for noise floor adaptation during silence, in `(0, 1]`.
    pub noise_floor_alpha: f32,
    pub noise_floor_min_dbfs: f32,
    pub noise_floor_max_dbfs: f32,
}

impl Default for VadConfig {
    fn default() -> Self {
        VadConfig {
            window_ms: 20,
            threshold_margin_db: 12.0,
            noise_floor_alpha: 0.05,
            noise_floor_min_dbfs: -70.0,
            noise_floor_max_dbfs: -20.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadDecision {
    Speech,
    Silence,
}

/// Classifies fixed-size windows of mono samples as speech or silence against a noise floor
/// that adapts slowly during silence. Not `Send`-restricted or stateful beyond `self` — one
/// instance is owned per `Segmenter`, one `Segmenter` per source.
pub struct EnergyVad {
    config: VadConfig,
    noise_floor_dbfs: f32,
    initialized: bool,
}

impl EnergyVad {
    pub fn new(config: VadConfig) -> Self {
        EnergyVad {
            config,
            noise_floor_dbfs: config.noise_floor_min_dbfs,
            initialized: false,
        }
    }

    pub fn window_len_samples(&self, sample_rate: u32) -> usize {
        ((sample_rate as u64 * self.config.window_ms as u64) / 1000) as usize
    }

    /// Classifies one window and, for silence windows, updates the adaptive noise floor.
    /// Only silence windows adjust the floor — letting speech drag it up would desensitize
    /// the detector over a long utterance.
    pub fn classify(&mut self, window: &[f32]) -> VadDecision {
        let level = rms_dbfs(window);

        if !self.initialized {
            self.noise_floor_dbfs = level.clamp(
                self.config.noise_floor_min_dbfs,
                self.config.noise_floor_max_dbfs,
            );
            self.initialized = true;
            return VadDecision::Silence; // first window only establishes the floor
        }

        let threshold = self.noise_floor_dbfs + self.config.threshold_margin_db;
        if level >= threshold {
            VadDecision::Speech
        } else {
            let alpha = self.config.noise_floor_alpha;
            self.noise_floor_dbfs = (self.noise_floor_dbfs * (1.0 - alpha) + level * alpha).clamp(
                self.config.noise_floor_min_dbfs,
                self.config.noise_floor_max_dbfs,
            );
            VadDecision::Silence
        }
    }

    pub fn noise_floor_dbfs(&self) -> f32 {
        self.noise_floor_dbfs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(len: usize, amplitude: f32) -> Vec<f32> {
        (0..len)
            .map(|i| if i % 2 == 0 { amplitude } else { -amplitude })
            .collect()
    }

    #[test]
    fn first_window_never_counts_as_speech() {
        let mut vad = EnergyVad::new(VadConfig::default());
        assert_eq!(vad.classify(&tone(320, 0.9)), VadDecision::Silence);
    }

    #[test]
    fn sustained_silence_stays_silence() {
        let mut vad = EnergyVad::new(VadConfig::default());
        let silence = vec![0.0; 320];
        for _ in 0..10 {
            assert_eq!(vad.classify(&silence), VadDecision::Silence);
        }
    }

    #[test]
    fn loud_window_after_silence_is_speech() {
        let mut vad = EnergyVad::new(VadConfig::default());
        vad.classify(&vec![0.0; 320]); // establish floor
        assert_eq!(vad.classify(&tone(320, 0.9)), VadDecision::Speech);
    }

    #[test]
    fn noise_floor_adapts_toward_a_louder_ambient_level_during_silence() {
        let mut vad = EnergyVad::new(VadConfig::default());
        let ambient = tone(320, 0.02); // quiet but not digital silence
        for _ in 0..200 {
            vad.classify(&ambient);
        }
        let floor = vad.noise_floor_dbfs();
        let ambient_level = rms_dbfs(&ambient);
        assert!(
            (floor - ambient_level).abs() < 1.0,
            "expected floor to converge near {ambient_level}, got {floor}"
        );
    }

    #[test]
    fn noise_floor_never_exceeds_configured_bounds() {
        let config = VadConfig::default();
        let mut vad = EnergyVad::new(config);
        // ~-15 dBFS: above `noise_floor_max_dbfs` (-20), but always classified as silence
        // once the floor approaches its cap (threshold = floor + 12dB stays above -15dB).
        // Without the clamp, the EMA would keep pulling the floor above -20 toward -15.
        let ambient_above_cap = tone(320, 0.178);
        for _ in 0..500 {
            vad.classify(&ambient_above_cap);
        }
        assert!(vad.noise_floor_dbfs() <= config.noise_floor_max_dbfs);
        assert!(vad.noise_floor_dbfs() >= config.noise_floor_min_dbfs);
    }
}
