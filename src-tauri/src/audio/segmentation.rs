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
use crate::audio::types::AudioSource;
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
        timestamp: AudioTimestamp,
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
}

impl Segmenter {
    pub fn new(source: AudioSource, sample_rate: u32, config: SegmentationConfig) -> Self {
        let vad = EnergyVad::new(config.vad);
        let window_len = vad.window_len_samples(sample_rate);
        let pre_roll_cap = ((sample_rate as u64 * config.pre_roll_ms as u64) / 1000) as usize;
        Segmenter {
            source,
            sample_rate,
            config,
            vad,
            window_len,
            pending: Vec::new(),
            pre_roll: VecDeque::with_capacity(pre_roll_cap),
            pre_roll_cap,
            samples_seen: 0,
            state: State::Idle,
        }
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
                    let duration_ms = (buffer.len() as u64 * 1000) / self.sample_rate as u64;
                    if duration_ms >= self.config.maximum_segment_ms as u64 {
                        let ended_at = self.timestamp_for(window_end_samples);
                        let segment = AudioSegment::new(
                            self.source,
                            buffer,
                            self.sample_rate,
                            started_at,
                            ended_at,
                        );
                        events.push(SpeechEvent::SegmentReady(segment));
                        events.push(SpeechEvent::Ended {
                            source: self.source,
                            timestamp: ended_at,
                        });
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
                    State::Speaking { buffer, started_at }
                } else {
                    let silence_windows = silence_windows + 1;
                    let silence_ms = silence_windows * self.config.vad.window_ms;
                    if silence_ms >= self.config.end_silence_ms {
                        let silence_samples = silence_windows as u64 * self.window_len as u64;
                        let keep_samples =
                            (self.config.post_roll_ms as u64 * self.sample_rate as u64) / 1000;
                        let trim_samples = silence_samples.saturating_sub(keep_samples);
                        let new_len = buffer.len().saturating_sub(trim_samples as usize);
                        buffer.truncate(new_len);
                        let ended_at =
                            self.timestamp_for(window_end_samples.saturating_sub(trim_samples));
                        let segment = AudioSegment::new(
                            self.source,
                            buffer,
                            self.sample_rate,
                            started_at,
                            ended_at,
                        );
                        events.push(SpeechEvent::SegmentReady(segment));
                        events.push(SpeechEvent::Ended {
                            source: self.source,
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
}
