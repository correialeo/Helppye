use serde::Serialize;

use crate::audio::error::AudioCaptureError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioSource {
    Microphone,
    SystemOutput,
}

#[derive(Debug, Clone, Serialize)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
    pub source: AudioSource,
    pub is_default: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AudioFrame {
    pub source: AudioSource,
    /// Mono, normalized to the pipeline's configured sample rate. See `CaptureConfig`.
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
    /// Monotonic milliseconds since the capture session started (not wall-clock time).
    pub timestamp_ms: u64,
}

/// Tagged with `source` on every variant (not just `Frame`/`Started`, which already carry
/// an `AudioDevice`) so the frontend can route events correctly when microphone and system
/// output capture run concurrently — two independent capture sessions share this one event
/// type, and a bare "stopped" or "error" would otherwise be ambiguous about which stopped.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AudioCaptureEvent {
    Started {
        device: AudioDevice,
    },
    Frame(AudioFrame),
    DeviceDisconnected {
        source: AudioSource,
        device_id: String,
    },
    Error {
        source: AudioSource,
        message: String,
    },
    Stopped {
        source: AudioSource,
    },
}

impl AudioCaptureEvent {
    pub fn error(source: AudioSource, err: AudioCaptureError) -> Self {
        AudioCaptureEvent::Error {
            source,
            message: err.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_serialize_with_a_type_tag_the_frontend_can_match_on() {
        let event = AudioCaptureEvent::Stopped {
            source: AudioSource::Microphone,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "stopped");
        assert_eq!(json["source"], "microphone");
    }

    #[test]
    fn capture_error_converts_to_a_tagged_error_event() {
        let event =
            AudioCaptureEvent::error(AudioSource::SystemOutput, AudioCaptureError::NoDeviceFound);
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "error");
        assert_eq!(json["source"], "system_output");
        assert_eq!(json["message"], "no matching audio device found");
    }

    #[test]
    fn frame_carries_source_and_normalized_format() {
        let frame = AudioFrame {
            source: AudioSource::Microphone,
            samples: vec![0.0; 1_600],
            sample_rate: 16_000,
            channels: 1,
            timestamp_ms: 0,
        };
        assert_eq!(frame.samples.len(), 1_600);
        assert_eq!(frame.channels, 1);
    }
}
