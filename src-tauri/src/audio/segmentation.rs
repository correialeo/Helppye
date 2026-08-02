//! Turns a stream of VAD decisions into bounded `AudioSegment`s, one `Segmenter` per source.
//!
//! State machine: `Idle` → `PossibleSpeech` → `Speaking` → `SilencePending` → back to `Idle`
//! (or, at `maximum_segment_ms`, `Speaking` finalizes and re-opens a fresh `Speaking` segment
//! without returning to `Idle`, since the source is presumably still talking). There is no
//! persistent "Completed" state — finalizing a segment is a transition action, not a state.
//!
//! Timestamps are derived from an internal `samples_seen` counter, not from
//! `AudioFrame::timestamp_ms`, so a dropped upstream frame can't skew segment boundaries.
//! Segment duration is likewise derived on demand from the accumulated buffer length rather
//! than a separately accumulated millisecond counter, for the same reason.

use std::collections::VecDeque;

use crate::audio::segment::{AudioSegment, AudioTimestamp};
use crate::audio::types::{AudioSource, CaptureStreamId};
use crate::audio::vad::{EnergyVad, VadConfig, VadDecision};

/// Initial, non-final tuning — see `docs/speech-segmentation.md`.
#[derive(Debug, Clone, Copy)]
pub struct SegmentationConfig {
    pub vad: VadConfig,
    /// Minimum sustained speech before a candidate is confirmed as a real segment (filters
    /// out brief blips like clicks or coughs).
    pub minimum_speech_ms: u32,
    /// Trailing silence required to consider a segment finished.
    pub end_silence_ms: u32,
    /// Hard cap on segment length; longer speech is cut here and continued in a new segment.
    pub maximum_segment_ms: u32,
    /// Audio kept from just before confirmed speech onset, so the segment doesn't clip the
    /// first syllable.
    pub pre_roll_ms: u32,
    /// Trailing silence kept in a finalized segment, trimmed down from `end_silence_ms`.
    pub post_roll_ms: u32,
}

impl Default for SegmentationConfig {
    fn default() -> Self {
        SegmentationConfig {
            vad: VadConfig::default(),
            minimum_speech_ms: 250,
            end_silence_ms: 600,
            maximum_segment_ms: 30_000,
            pre_roll_ms: 200,
            post_roll_ms: 100,
        }
    }
}

#[derive(Debug, Clone)]
pub enum SpeechEvent {
    Started {
        source: AudioSource,
        timestamp: AudioTimestamp,
    },
    Ended {
        source: AudioSource,
        /// End of detected voice before retained post-roll.
        speech_ended_at: AudioTimestamp,
        /// End of audio retained for transcription (may include post-roll).
        timestamp: AudioTimestamp,
    },
    /// Audio that belongs to a confirmed local speech activity. Emitted only when
    /// `with_streaming_audio_events` is enabled; batch providers keep the old zero-copy
    /// behavior.
    Audio {
        source: AudioSource,
        samples: Vec<f32>,
        started_at: AudioTimestamp,
        ended_at: AudioTimestamp,
    },
    SegmentReady(AudioSegment),
}

enum State {
    Idle,
    PossibleSpeech {
        buffer: Vec<f32>,
        speech_windows: u32,
        candidate_start_samples: u64,
    },
    Speaking {
        buffer: Vec<f32>,
        started_at: AudioTimestamp,
    },
    SilencePending {
        buffer: Vec<f32>,
        started_at: AudioTimestamp,
        silence_windows: u32,
    },
}

/// Consumes arbitrary-length sample chunks for one source and emits `SpeechEvent`s. Never
/// mixes samples across sources — one instance per `AudioSource`.
pub struct Segmenter {
    source: AudioSource,
    capture_stream_id: CaptureStreamId,
    next_sequence: u64,
    sample_rate: u32,
    config: SegmentationConfig,
    vad: EnergyVad,
    window_len: usize,
    /// Samples not yet forming a full VAD window.
    pending: Vec<f32>,
    pre_roll: VecDeque<f32>,
    pre_roll_cap: usize,
    samples_seen: u64,
    state: State,
    emit_streaming_audio: bool,
}

impl Segmenter {
    pub fn new(source: AudioSource, sample_rate: u32, config: SegmentationConfig) -> Self {
        Segmenter::for_stream(source, CaptureStreamId::UNASSIGNED, sample_rate, config)
    }

    /// Segmentador ligado a um fluxo físico de captura concreto. Todo segmento que sair
    /// daqui nasce carimbado com `capture_stream_id` e um `sequence_number` monotônico —
    /// a origem é decidida **uma vez**, aqui, e nunca reinferida adiante no pipeline.
    pub fn for_stream(
        source: AudioSource,
        capture_stream_id: CaptureStreamId,
        sample_rate: u32,
        config: SegmentationConfig,
    ) -> Self {
        let vad = EnergyVad::new(config.vad);
        let window_len = vad.window_len_samples(sample_rate);
        let pre_roll_cap = ((sample_rate as u64 * config.pre_roll_ms as u64) / 1000) as usize;
        Segmenter {
            source,
            capture_stream_id,
            next_sequence: 0,
            sample_rate,
            config,
            vad,
            window_len,
            pending: Vec::new(),
            pre_roll: VecDeque::with_capacity(pre_roll_cap),
            pre_roll_cap,
            samples_seen: 0,
            state: State::Idle,
            emit_streaming_audio: false,
        }
    }

    pub fn with_streaming_audio_events(mut self) -> Self {
        self.emit_streaming_audio = true;
        self
    }

    fn emit_streaming_audio(
        &self,
        events: &mut Vec<SpeechEvent>,
        samples: &[f32],
        start_sample: u64,
    ) {
        if !self.emit_streaming_audio || samples.is_empty() {
            return;
        }
        events.push(SpeechEvent::Audio {
            source: self.source,
            samples: samples.to_vec(),
            started_at: self.timestamp_for(start_sample),
            ended_at: self.timestamp_for(start_sample + samples.len() as u64),
        });
    }

    fn mint_segment(
        &mut self,
        buffer: Vec<f32>,
        started_at: AudioTimestamp,
        ended_at: AudioTimestamp,
    ) -> AudioSegment {
        self.next_sequence += 1;
        AudioSegment::new(self.source, buffer, self.sample_rate, started_at, ended_at)
            .in_stream(self.capture_stream_id, self.next_sequence)
    }

    /// Feeds mono samples at `sample_rate`, processing every full VAD window they complete.
    /// Non-blocking — pure computation, safe to call from any thread.
    pub fn push_samples(&mut self, samples: &[f32]) -> Vec<SpeechEvent> {
        self.pending.extend_from_slice(samples);
        let mut events = Vec::new();
        while self.pending.len() >= self.window_len {
            let window: Vec<f32> = self.pending.drain(0..self.window_len).collect();
            self.process_window(&window, &mut events);
        }
        events
    }

    /// Flushes a confirmed speech segment when its source ends. Without this, stopping
    /// capture or benchmarking a fixture that ends immediately after speech silently
    /// loses the final phrase because no trailing VAD window arrives.
    pub fn finish(&mut self) -> Vec<SpeechEvent> {
        let mut events = Vec::new();
        let pending = std::mem::take(&mut self.pending);
        let pending_start_samples = self.samples_seen;
        self.samples_seen += pending.len() as u64;
        let ended_at = self.timestamp_for(self.samples_seen);

        let state = std::mem::replace(&mut self.state, State::Idle);
        let finalized = match state {
            State::Speaking {
                mut buffer,
                started_at,
            } => {
                self.emit_streaming_audio(&mut events, &pending, pending_start_samples);
                buffer.extend_from_slice(&pending);
                Some((buffer, started_at, ended_at, ended_at))
            }
            State::SilencePending {
                mut buffer,
                started_at,
                silence_windows,
            } => {
                buffer.extend_from_slice(&pending);
                let silence_samples = silence_windows as usize * self.window_len;
                let keep_samples =
                    (self.config.post_roll_ms as usize * self.sample_rate as usize) / 1_000;
                let keep_samples = keep_samples.min(silence_samples);
                let streaming_silence_samples = silence_samples + pending.len();
                let streaming_keep_samples =
                    (keep_samples + pending.len()).min(streaming_silence_samples);
                let silence_start = buffer.len().saturating_sub(streaming_silence_samples);
                self.emit_streaming_audio(
                    &mut events,
                    &buffer[silence_start..silence_start + streaming_keep_samples],
                    self.samples_seen
                        .saturating_sub(streaming_silence_samples as u64),
                );
                let trim_samples = silence_samples.saturating_sub(keep_samples);
                buffer.truncate(buffer.len().saturating_sub(trim_samples));
                let trimmed_end =
                    self.timestamp_for(self.samples_seen.saturating_sub(trim_samples as u64));
                let speech_ended_at = self.timestamp_for(
                    self.samples_seen
                        .saturating_sub(streaming_silence_samples as u64),
                );
                Some((buffer, started_at, trimmed_end, speech_ended_at))
            }
            State::Idle | State::PossibleSpeech { .. } => None,
        };

        self.pre_roll.clear();
        if let Some((buffer, started_at, ended_at, speech_ended_at)) = finalized {
            if !buffer.is_empty() {
                let segment = self.mint_segment(buffer, started_at, ended_at);
                events.push(SpeechEvent::SegmentReady(segment));
                events.push(SpeechEvent::Ended {
                    source: self.source,
                    speech_ended_at,
                    timestamp: ended_at,
                });
            }
        }
        events
    }

    fn timestamp_for(&self, samples: u64) -> AudioTimestamp {
        AudioTimestamp(samples * 1000 / self.sample_rate as u64)
    }

    fn push_pre_roll(&mut self, window: &[f32]) {
        self.pre_roll.extend(window.iter().copied());
        while self.pre_roll.len() > self.pre_roll_cap {
            self.pre_roll.pop_front();
        }
    }

    fn process_window(&mut self, window: &[f32], events: &mut Vec<SpeechEvent>) {
        let window_start_samples = self.samples_seen;
        let decision = self.vad.classify(window);
        self.samples_seen += window.len() as u64;
        let window_end_samples = self.samples_seen;

        self.state = match std::mem::replace(&mut self.state, State::Idle) {
            State::Idle => {
                if decision == VadDecision::Speech {
                    State::PossibleSpeech {
                        buffer: window.to_vec(),
                        speech_windows: 1,
                        candidate_start_samples: window_start_samples,
                    }
                } else {
                    self.push_pre_roll(window);
                    State::Idle
                }
            }

            State::PossibleSpeech {
                mut buffer,
                speech_windows,
                candidate_start_samples,
            } => {
                if decision == VadDecision::Speech {
                    buffer.extend_from_slice(window);
                    let speech_windows = speech_windows + 1;
                    let speech_ms = speech_windows * self.config.vad.window_ms;
                    if speech_ms >= self.config.minimum_speech_ms {
                        let pre_roll: Vec<f32> = self.pre_roll.iter().copied().collect();
                        let started_at = self.timestamp_for(
                            candidate_start_samples.saturating_sub(pre_roll.len() as u64),
                        );
                        let mut full_buffer = pre_roll;
                        full_buffer.extend_from_slice(&buffer);
                        self.pre_roll.clear();
                        events.push(SpeechEvent::Started {
                            source: self.source,
                            timestamp: started_at,
                        });
                        self.emit_streaming_audio(
                            events,
                            &full_buffer,
                            candidate_start_samples.saturating_sub(
                                full_buffer.len().saturating_sub(buffer.len()) as u64,
                            ),
                        );
                        State::Speaking {
                            buffer: full_buffer,
                            started_at,
                        }
                    } else {
                        State::PossibleSpeech {
                            buffer,
                            speech_windows,
                            candidate_start_samples,
                        }
                    }
                } else {
                    // False positive: discard the candidate. Pre-roll is reseeded with just
                    // this window rather than the full discarded candidate, so there's a
                    // small gap in pre-roll continuity right after a discarded blip — an
                    // accepted simplification, not a bug.
                    self.pre_roll.clear();
                    self.push_pre_roll(window);
                    State::Idle
                }
            }

            State::Speaking {
                mut buffer,
                started_at,
            } => {
                buffer.extend_from_slice(window);
                if decision == VadDecision::Silence {
                    State::SilencePending {
                        buffer,
                        started_at,
                        silence_windows: 1,
                    }
                } else {
                    self.emit_streaming_audio(events, window, window_start_samples);
                    let duration_ms = (buffer.len() as u64 * 1000) / self.sample_rate as u64;
                    if duration_ms >= self.config.maximum_segment_ms as u64 {
                        let ended_at = self.timestamp_for(window_end_samples);
                        let segment = self.mint_segment(buffer, started_at, ended_at);
                        events.push(SpeechEvent::SegmentReady(segment));
                        State::Speaking {
                            buffer: Vec::new(),
                            started_at: ended_at,
                        }
                    } else {
                        State::Speaking { buffer, started_at }
                    }
                }
            }

            State::SilencePending {
                mut buffer,
                started_at,
                silence_windows,
            } => {
                buffer.extend_from_slice(window);
                if decision == VadDecision::Speech {
                    let resumed_samples = silence_windows as usize * self.window_len + window.len();
                    let resumed_start = buffer.len().saturating_sub(resumed_samples);
                    self.emit_streaming_audio(
                        events,
                        &buffer[resumed_start..],
                        window_end_samples.saturating_sub(resumed_samples as u64),
                    );
                    State::Speaking { buffer, started_at }
                } else {
                    let silence_windows = silence_windows + 1;
                    let silence_ms = silence_windows * self.config.vad.window_ms;
                    if silence_ms >= self.config.end_silence_ms {
                        let silence_samples = silence_windows as u64 * self.window_len as u64;
                        let keep_samples =
                            (self.config.post_roll_ms as u64 * self.sample_rate as u64) / 1000;
                        let keep_samples = keep_samples.min(silence_samples);
                        let silence_start = buffer.len().saturating_sub(silence_samples as usize);
                        self.emit_streaming_audio(
                            events,
                            &buffer[silence_start..silence_start + keep_samples as usize],
                            window_end_samples.saturating_sub(silence_samples),
                        );
                        let trim_samples = silence_samples.saturating_sub(keep_samples);
                        let new_len = buffer.len().saturating_sub(trim_samples as usize);
                        buffer.truncate(new_len);
                        let ended_at =
                            self.timestamp_for(window_end_samples.saturating_sub(trim_samples));
                        let speech_ended_at =
                            self.timestamp_for(window_end_samples.saturating_sub(silence_samples));
                        let segment = self.mint_segment(buffer, started_at, ended_at);
                        events.push(SpeechEvent::SegmentReady(segment));
                        events.push(SpeechEvent::Ended {
                            source: self.source,
                            speech_ended_at,
                            timestamp: ended_at,
                        });
                        self.pre_roll.clear();
                        State::Idle
                    } else {
                        State::SilencePending {
                            buffer,
                            started_at,
                            silence_windows,
                        }
                    }
                }
            }
        };
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

    fn segment_ready_events(events: &[SpeechEvent]) -> Vec<&AudioSegment> {
        events
            .iter()
            .filter_map(|e| match e {
                SpeechEvent::SegmentReady(seg) => Some(seg),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn happy_path_produces_one_segment_with_preroll_and_trimmed_postroll() {
        let mut seg = Segmenter::new(
            AudioSource::SystemOutput,
            16_000,
            SegmentationConfig::default(),
        );
        let silence = vec![0.0f32; 320];
        let speech = tone(320, 0.9);

        let mut events = Vec::new();
        for _ in 0..5 {
            events.extend(seg.push_samples(&silence));
        }
        for _ in 0..20 {
            events.extend(seg.push_samples(&speech));
        }
        for _ in 0..35 {
            events.extend(seg.push_samples(&silence));
        }

        let started: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, SpeechEvent::Started { .. }))
            .collect();
        assert_eq!(started.len(), 1);
        assert!(matches!(
            started[0],
            SpeechEvent::Started { timestamp, .. } if timestamp.0 == 0
        ));

        let segments = segment_ready_events(&events);
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].samples.len(), 9_600);
        assert_eq!(segments[0].duration_ms, 600);
        assert_eq!(segments[0].started_at.0, 0);
        assert_eq!(segments[0].ended_at.0, 600);
        assert_eq!(segments[0].source, AudioSource::SystemOutput);

        let ended: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, SpeechEvent::Ended { .. }))
            .collect();
        assert_eq!(ended.len(), 1);
        assert!(matches!(
            ended[0],
            SpeechEvent::Ended {
                speech_ended_at,
                timestamp,
                ..
            } if speech_ended_at.0 == 500 && timestamp.0 == 600
        ));
    }

    #[test]
    fn forced_completion_at_max_duration_continues_the_segment() {
        let config = SegmentationConfig {
            vad: VadConfig::default(),
            minimum_speech_ms: 0,
            pre_roll_ms: 0,
            post_roll_ms: 0,
            end_silence_ms: 10_000,
            maximum_segment_ms: 500,
        };
        let mut seg = Segmenter::new(AudioSource::Microphone, 16_000, config);
        let silence = vec![0.0f32; 320];
        let speech = tone(320, 0.9);

        let mut events = seg.push_samples(&silence); // establish a stable noise floor first
        for _ in 0..60 {
            events.extend(seg.push_samples(&speech));
        }

        let segments = segment_ready_events(&events);
        assert_eq!(segments.len(), 2);

        assert_eq!(segments[0].samples.len(), 8_000);
        assert_eq!(segments[0].started_at.0, 20);
        assert_eq!(segments[0].ended_at.0, 520);
        assert_eq!(segments[0].duration_ms, 500);

        assert_eq!(segments[1].samples.len(), 8_000);
        assert_eq!(segments[1].started_at.0, 520);
        assert_eq!(segments[1].ended_at.0, 1_020);
        assert_eq!(segments[1].duration_ms, 500);
    }

    #[test]
    fn brief_blip_is_discarded_without_emitting_a_segment() {
        let config = SegmentationConfig {
            vad: VadConfig::default(),
            minimum_speech_ms: 250,
            ..SegmentationConfig::default()
        };
        let mut seg = Segmenter::new(AudioSource::Microphone, 16_000, config);
        let silence = vec![0.0f32; 320];
        let speech = tone(320, 0.9);

        let mut events = Vec::new();
        for _ in 0..5 {
            events.extend(seg.push_samples(&silence));
        }
        // Only 2 windows (40ms) of speech: well under minimum_speech_ms (250ms).
        for _ in 0..2 {
            events.extend(seg.push_samples(&speech));
        }
        for _ in 0..10 {
            events.extend(seg.push_samples(&silence));
        }

        assert!(segment_ready_events(&events).is_empty());
        assert!(!events
            .iter()
            .any(|e| matches!(e, SpeechEvent::Started { .. })));
    }

    #[test]
    fn finish_preserves_confirmed_speech_without_trailing_silence() {
        let mut segmenter = Segmenter::new(
            AudioSource::SystemOutput,
            16_000,
            SegmentationConfig::default(),
        );
        let speech = tone(320, 0.9);
        let mut before_finish = Vec::new();
        for _ in 0..20 {
            before_finish.extend(segmenter.push_samples(&speech));
        }
        assert!(segment_ready_events(&before_finish).is_empty());

        let finished = segmenter.finish();
        let segments = segment_ready_events(&finished);
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].source, AudioSource::SystemOutput);
        assert!(segments[0].duration_ms >= 250);
        assert!(finished
            .iter()
            .any(|event| matches!(event, SpeechEvent::Ended { .. })));
    }

    #[test]
    fn streaming_events_preserve_preroll_order_and_every_segment_sample_once() {
        let mut segmenter = Segmenter::new(
            AudioSource::SystemOutput,
            16_000,
            SegmentationConfig::default(),
        )
        .with_streaming_audio_events();
        let silence = vec![0.0; 320];
        let speech = tone(320, 0.9);
        let mut events = Vec::new();
        for _ in 0..10 {
            events.extend(segmenter.push_samples(&silence));
        }
        for _ in 0..20 {
            events.extend(segmenter.push_samples(&speech));
        }
        for _ in 0..30 {
            events.extend(segmenter.push_samples(&silence));
        }

        let start = events
            .iter()
            .position(|event| matches!(event, SpeechEvent::Started { .. }))
            .unwrap();
        let first_audio = events
            .iter()
            .position(|event| matches!(event, SpeechEvent::Audio { .. }))
            .unwrap();
        let end = events
            .iter()
            .position(|event| matches!(event, SpeechEvent::Ended { .. }))
            .unwrap();
        let last_audio = events
            .iter()
            .rposition(|event| matches!(event, SpeechEvent::Audio { .. }))
            .unwrap();
        assert!(start < first_audio);
        assert!(last_audio < end);

        let streamed: Vec<f32> = events
            .iter()
            .filter_map(|event| match event {
                SpeechEvent::Audio { samples, .. } => Some(samples.as_slice()),
                _ => None,
            })
            .flatten()
            .copied()
            .collect();
        let segment = segment_ready_events(&events).into_iter().next().unwrap();
        assert_eq!(streamed, segment.samples);
    }
}
