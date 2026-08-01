use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::audio::error::AudioCaptureError;

static NEXT_CAPTURE_STREAM_ID: AtomicU64 = AtomicU64::new(1);

/// Identifica **um fluxo físico contínuo de captura**: um `start_capture` de uma fonte até
/// o `stop_capture`/desconexão correspondente. Trocar de microfone no meio da sessão encerra
/// um fluxo e abre outro, ainda que a `AudioSource` e a `SessionId` sejam as mesmas.
///
/// Existe porque `AudioSource` sozinha não distingue dois fluxos da mesma categoria: o
/// relógio de `AudioTimestamp` é reiniciado a cada fluxo, e a fila de identidades pendentes
/// da transcrição (`TranscriptionStreamKey`) precisa saber que o resultado que chegou
/// pertence ao fluxo que o produziu e não ao anterior, cujos segmentos ainda podem estar em
/// voo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CaptureStreamId(u64);

impl CaptureStreamId {
    /// Fluxo desconhecido — só para segmentos construídos fora de uma sessão real de
    /// captura (testes, harness de benchmark, fixtures). Nunca é emitido por
    /// `CaptureEngine::start_capture`, que sempre cunha um id real.
    pub const UNASSIGNED: CaptureStreamId = CaptureStreamId(0);

    pub fn next() -> Self {
        CaptureStreamId(NEXT_CAPTURE_STREAM_ID.fetch_add(1, Ordering::Relaxed))
    }

    pub fn value(self) -> u64 {
        self.0
    }
}

/// `Deserialize` além de `Serialize` porque a fonte também **entra** no processo, não só
/// sai: o manifesto de fixtures do harness de benchmark declara de que lado da conversa cada
/// áudio veio, e essa declaração não pode ser inferida do arquivo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
