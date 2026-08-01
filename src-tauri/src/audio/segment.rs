//! Segment types shared by the segmentation state machine and the transcription layer.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

use crate::audio::types::{AudioSource, CaptureStreamId};

/// Monotonic milliseconds since the owning capture session started — the same clock as
/// `AudioFrame::timestamp_ms`, never wall-clock time. Only comparable within one capture
/// session for one source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct AudioTimestamp(pub u64);

impl AudioTimestamp {
    pub fn saturating_sub(self, other: AudioTimestamp) -> u64 {
        self.0.saturating_sub(other.0)
    }

    pub fn saturating_add_ms(self, offset_ms: u64) -> Self {
        AudioTimestamp(self.0.saturating_add(offset_ms))
    }
}

static NEXT_SEGMENT_ID: AtomicU64 = AtomicU64::new(1);

/// Opaque, process-unique identifier for one detected speech segment. Not stable across
/// restarts and not meaningful across sources — only used to correlate a segment with its
/// eventual `Transcript`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct SegmentId(u64);

impl SegmentId {
    pub fn next() -> Self {
        SegmentId(NEXT_SEGMENT_ID.fetch_add(1, Ordering::Relaxed))
    }

    /// Valor bruto, para compor identificadores derivados (ex.: o `provider_event_id`
    /// determinístico de um resultado de transcrição). Não use para ordenar: ids são
    /// únicos por processo, mas a ordem de emissão entre fontes não é significativa.
    pub fn value(self) -> u64 {
        self.0
    }
}

/// One bounded span of detected speech from a single source, ready for transcription.
/// Samples are always mono `f32` at `sample_rate`, and always from exactly one
/// `AudioSource` — segments are never mixed across microphone and system output.
#[derive(Debug, Clone)]
pub struct AudioSegment {
    pub id: SegmentId,
    pub source: AudioSource,
    /// Fluxo físico de captura que produziu este segmento. Junto de `sequence_number`,
    /// é a parte da identidade causal que sobrevive até a Conversation Timeline — ver
    /// `transcription::envelope::TranscriptionWorkItem`.
    pub capture_stream_id: CaptureStreamId,
    /// Posição deste segmento dentro do seu `capture_stream_id`, começando em 1. Monotônico
    /// por fluxo e nunca comparável entre fluxos.
    pub sequence_number: u64,
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub started_at: AudioTimestamp,
    pub ended_at: AudioTimestamp,
    pub duration_ms: u64,
}

impl AudioSegment {
    pub fn new(
        source: AudioSource,
        samples: Vec<f32>,
        sample_rate: u32,
        started_at: AudioTimestamp,
        ended_at: AudioTimestamp,
    ) -> Self {
        AudioSegment {
            id: SegmentId::next(),
            source,
            capture_stream_id: CaptureStreamId::UNASSIGNED,
            sequence_number: 0,
            samples,
            sample_rate,
            started_at,
            ended_at,
            duration_ms: ended_at.saturating_sub(started_at),
        }
    }

    /// Carimba o segmento com a identidade do fluxo que o produziu. Usado pelo `Segmenter`,
    /// que é a única coisa que sabe de qual `start_capture` este áudio saiu.
    pub fn in_stream(mut self, capture_stream_id: CaptureStreamId, sequence_number: u64) -> Self {
        self.capture_stream_id = capture_stream_id;
        self.sequence_number = sequence_number;
        self
    }

    /// Shifts segment-local timestamps onto a broader monotonic timeline. Used by the
    /// capture engine to make independently-started microphone and system-output sessions
    /// comparable before transcription and conversation ordering.
    pub fn with_timestamp_offset(mut self, offset_ms: u64) -> Self {
        self.started_at = self.started_at.saturating_add_ms(offset_ms);
        self.ended_at = self.ended_at.saturating_add_ms(offset_ms);
        self.duration_ms = self.ended_at.saturating_sub(self.started_at);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_ids_are_unique() {
        let a = SegmentId::next();
        let b = SegmentId::next();
        assert_ne!(a, b);
    }

    #[test]
    fn duration_is_derived_from_timestamps() {
        let seg = AudioSegment::new(
            AudioSource::SystemOutput,
            vec![0.0; 1_600],
            16_000,
            AudioTimestamp(1_000),
            AudioTimestamp(1_800),
        );
        assert_eq!(seg.duration_ms, 800);
        assert_eq!(seg.source, AudioSource::SystemOutput);
    }
}
