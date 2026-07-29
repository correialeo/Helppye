//! Conversation Timeline and turn assembly.
//!
//! `TranscriptSegment` is the raw output of local transcription. `ConversationTurn` is a
//! logical utterance from one speaker, built from one or more consecutive transcript
//! segments. This layer is intentionally text-only: it does not correct transcription,
//! summarize, call an LLM, or discard raw segment IDs.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use tracing::{info, warn};

use crate::audio::segment::{AudioTimestamp, SegmentId};
use crate::audio::types::{AudioCaptureEvent, AudioSource};
use crate::transcription::types::{Transcript, TranscriptEvent};

pub const CONVERSATION_TIMELINE_EVENT: &str = "conversation://timeline-event";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationSpeaker {
    User,
    OtherPerson,
}

impl From<AudioSource> for ConversationSpeaker {
    fn from(source: AudioSource) -> Self {
        match source {
            AudioSource::Microphone => ConversationSpeaker::User,
            AudioSource::SystemOutput => ConversationSpeaker::OtherPerson,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct TurnId(u64);

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TranscriptSegment {
    pub segment_id: SegmentId,
    pub speaker: ConversationSpeaker,
    pub source: AudioSource,
    pub text: String,
    pub language: Option<String>,
    pub started_at: AudioTimestamp,
    pub ended_at: AudioTimestamp,
    pub processing_time_ms: u64,
    pub received_sequence: u64,
}

impl TranscriptSegment {
    fn from_transcript(transcript: Transcript, received_sequence: u64) -> Option<Self> {
        let text = normalize_segment_text(&transcript.text);
        if text.is_empty() {
            return None;
        }

        Some(TranscriptSegment {
            segment_id: transcript.segment_id,
            speaker: ConversationSpeaker::from(transcript.source),
            source: transcript.source,
            text,
            language: transcript.language,
            started_at: transcript.started_at,
            ended_at: transcript.ended_at,
            processing_time_ms: transcript.processing_time_ms,
            received_sequence,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConversationTurn {
    pub id: TurnId,
    pub speaker: ConversationSpeaker,
    pub source: AudioSource,
    pub text: String,
    pub segments: Vec<SegmentId>,
    pub started_at: AudioTimestamp,
    pub ended_at: AudioTimestamp,
    pub finalized_at: Option<AudioTimestamp>,
}

impl ConversationTurn {
    fn start(id: TurnId, segment: &TranscriptSegment) -> Self {
        ConversationTurn {
            id,
            speaker: segment.speaker,
            source: segment.source,
            text: segment.text.clone(),
            segments: vec![segment.segment_id],
            started_at: segment.started_at,
            ended_at: segment.ended_at,
            finalized_at: None,
        }
    }

    fn append(&mut self, segment: &TranscriptSegment) {
        self.text = join_turn_text(&self.text, &segment.text);
        self.segments.push(segment.segment_id);
        self.ended_at = std::cmp::max(self.ended_at, segment.ended_at);
    }

    fn duration_ms(&self) -> u64 {
        self.ended_at.saturating_sub(self.started_at)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FinalizationReason {
    SpeakerChanged,
    SourceChanged,
    GapExceeded,
    Paused,
    SessionEnded,
    ManualFlush,
    MaximumDuration,
}

impl FinalizationReason {
    fn as_str(self) -> &'static str {
        match self {
            FinalizationReason::SpeakerChanged => "speaker_changed",
            FinalizationReason::SourceChanged => "source_changed",
            FinalizationReason::GapExceeded => "gap_exceeded",
            FinalizationReason::Paused => "paused",
            FinalizationReason::SessionEnded => "session_ended",
            FinalizationReason::ManualFlush => "manual_flush",
            FinalizationReason::MaximumDuration => "maximum_duration",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TurnEvent {
    Started {
        turn_id: TurnId,
        speaker: ConversationSpeaker,
        source: AudioSource,
        started_at: AudioTimestamp,
    },
    Updated {
        turn_id: TurnId,
        speaker: ConversationSpeaker,
        source: AudioSource,
        started_at: AudioTimestamp,
        text: String,
        ended_at: AudioTimestamp,
        segments: Vec<SegmentId>,
    },
    Finalized {
        turn: ConversationTurn,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct TurnAssemblerConfig {
    pub same_speaker_merge_gap_ms: u64,
    pub maximum_turn_duration_ms: u64,
    pub out_of_order_tolerance_ms: u64,
}

impl Default for TurnAssemblerConfig {
    fn default() -> Self {
        TurnAssemblerConfig {
            same_speaker_merge_gap_ms: 1_800,
            maximum_turn_duration_ms: 120_000,
            out_of_order_tolerance_ms: 1_000,
        }
    }
}

struct TurnAssembler {
    config: TurnAssemblerConfig,
    open_turn: Option<ConversationTurn>,
    finalized_turns: Vec<ConversationTurn>,
    raw_segments: Vec<TranscriptSegment>,
    next_turn_id: u64,
    last_finalized_ended_at: Option<AudioTimestamp>,
}

impl TurnAssembler {
    fn new(config: TurnAssemblerConfig) -> Self {
        TurnAssembler {
            config,
            open_turn: None,
            finalized_turns: Vec::new(),
            raw_segments: Vec::new(),
            next_turn_id: 0,
            last_finalized_ended_at: None,
        }
    }

    fn ingest_segment(&mut self, segment: TranscriptSegment) -> Vec<TurnEvent> {
        self.warn_if_out_of_order(&segment);
        self.raw_segments.push(segment.clone());

        let mut events = Vec::new();
        if self.open_turn.is_none() {
            self.start_turn(segment, &mut events);
            return events;
        }

        let decision = {
            let open = self.open_turn.as_ref().expect("checked above");
            self.merge_decision(open, &segment)
        };

        match decision {
            MergeDecision::Append => {
                let open = self.open_turn.as_mut().expect("open turn exists");
                open.append(&segment);
                info!(
                    turn_id = open.id.0,
                    segments = open.segments.len(),
                    duration_ms = open.duration_ms(),
                    "conversation turn updated"
                );
                events.push(TurnEvent::Updated {
                    turn_id: open.id,
                    speaker: open.speaker,
                    source: open.source,
                    started_at: open.started_at,
                    text: open.text.clone(),
                    ended_at: open.ended_at,
                    segments: open.segments.clone(),
                });

                if open.duration_ms() >= self.config.maximum_turn_duration_ms {
                    events.extend(self.finalize_open(FinalizationReason::MaximumDuration));
                }
            }
            MergeDecision::StartNew(reason) => {
                events.extend(self.finalize_open(reason));
                self.start_turn(segment, &mut events);
            }
        }

        events
    }

    fn merge_decision(
        &self,
        open: &ConversationTurn,
        segment: &TranscriptSegment,
    ) -> MergeDecision {
        if open.speaker != segment.speaker {
            return MergeDecision::StartNew(FinalizationReason::SpeakerChanged);
        }
        if open.source != segment.source {
            return MergeDecision::StartNew(FinalizationReason::SourceChanged);
        }
        if segment.started_at < open.ended_at {
            let skew = open.ended_at.saturating_sub(segment.started_at);
            if skew > self.config.out_of_order_tolerance_ms {
                return MergeDecision::StartNew(FinalizationReason::GapExceeded);
            }
        }
        if segment.started_at > open.ended_at {
            let gap = segment.started_at.saturating_sub(open.ended_at);
            if gap > self.config.same_speaker_merge_gap_ms {
                return MergeDecision::StartNew(FinalizationReason::GapExceeded);
            }
        }
        let resulting_duration =
            std::cmp::max(open.ended_at, segment.ended_at).saturating_sub(open.started_at);
        if resulting_duration > self.config.maximum_turn_duration_ms {
            return MergeDecision::StartNew(FinalizationReason::MaximumDuration);
        }
        MergeDecision::Append
    }

    fn start_turn(&mut self, segment: TranscriptSegment, events: &mut Vec<TurnEvent>) {
        self.next_turn_id += 1;
        let turn = ConversationTurn::start(TurnId(self.next_turn_id), &segment);
        info!(
            turn_id = turn.id.0,
            speaker = ?turn.speaker,
            segment_id = ?segment.segment_id,
            "conversation turn started"
        );
        events.push(TurnEvent::Started {
            turn_id: turn.id,
            speaker: turn.speaker,
            source: turn.source,
            started_at: turn.started_at,
        });
        events.push(TurnEvent::Updated {
            turn_id: turn.id,
            speaker: turn.speaker,
            source: turn.source,
            started_at: turn.started_at,
            text: turn.text.clone(),
            ended_at: turn.ended_at,
            segments: turn.segments.clone(),
        });
        self.open_turn = Some(turn);

        if segment.ended_at.saturating_sub(segment.started_at)
            >= self.config.maximum_turn_duration_ms
        {
            events.extend(self.finalize_open(FinalizationReason::MaximumDuration));
        }
    }

    fn finalize_open(&mut self, reason: FinalizationReason) -> Vec<TurnEvent> {
        let Some(mut turn) = self.open_turn.take() else {
            return Vec::new();
        };
        turn.finalized_at = Some(turn.ended_at);
        self.last_finalized_ended_at = Some(turn.ended_at);
        info!(
            turn_id = turn.id.0,
            speaker = ?turn.speaker,
            segments = turn.segments.len(),
            reason = reason.as_str(),
            "conversation turn finalized"
        );
        self.finalized_turns.push(turn.clone());
        vec![TurnEvent::Finalized { turn }]
    }

    fn finalize_source(
        &mut self,
        source: AudioSource,
        reason: FinalizationReason,
    ) -> Vec<TurnEvent> {
        if self
            .open_turn
            .as_ref()
            .is_some_and(|turn| turn.source == source)
        {
            self.finalize_open(reason)
        } else {
            Vec::new()
        }
    }

    fn flush(&mut self) -> Vec<TurnEvent> {
        self.finalize_open(FinalizationReason::ManualFlush)
    }

    fn end_session(&mut self) -> Vec<TurnEvent> {
        self.finalize_open(FinalizationReason::SessionEnded)
    }

    fn snapshot(&self) -> Vec<ConversationTurn> {
        let mut turns = self.finalized_turns.clone();
        if let Some(open) = self.open_turn.clone() {
            turns.push(open);
        }
        turns.sort_by_key(|turn| (turn.started_at, turn.ended_at, turn.id));
        turns
    }

    fn raw_segments(&self) -> Vec<TranscriptSegment> {
        let mut segments = self.raw_segments.clone();
        segments.sort_by_key(|segment| (segment.started_at, segment.received_sequence));
        segments
    }

    fn warn_if_out_of_order(&self, segment: &TranscriptSegment) {
        if let Some(open) = &self.open_turn {
            if segment.started_at < open.ended_at {
                let skew = open.ended_at.saturating_sub(segment.started_at);
                warn!(
                    segment_id = ?segment.segment_id,
                    turn_id = open.id.0,
                    skew_ms = skew,
                    tolerance_ms = self.config.out_of_order_tolerance_ms,
                    "out-of-order transcript segment received while turn is open"
                );
            }
        }
        if let Some(last_finalized) = self.last_finalized_ended_at {
            if segment.started_at < last_finalized {
                let skew = last_finalized.saturating_sub(segment.started_at);
                warn!(
                    segment_id = ?segment.segment_id,
                    skew_ms = skew,
                    tolerance_ms = self.config.out_of_order_tolerance_ms,
                    "out-of-order transcript segment received after finalized turn"
                );
            }
        }
    }
}

enum MergeDecision {
    Append,
    StartNew(FinalizationReason),
}

pub struct ConversationTimeline {
    assembler: Mutex<TurnAssembler>,
    next_sequence: AtomicU64,
}

impl Default for ConversationTimeline {
    fn default() -> Self {
        ConversationTimeline::new(TurnAssemblerConfig::default())
    }
}

impl ConversationTimeline {
    pub fn new(config: TurnAssemblerConfig) -> Self {
        ConversationTimeline {
            assembler: Mutex::new(TurnAssembler::new(config)),
            next_sequence: AtomicU64::new(0),
        }
    }

    pub fn ingest_transcript_event(&self, event: TranscriptEvent) -> Vec<TurnEvent> {
        let TranscriptEvent::Ready(transcript) = event else {
            return Vec::new();
        };

        let sequence = self.next_sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let Some(segment) = TranscriptSegment::from_transcript(transcript, sequence) else {
            return Vec::new();
        };

        self.assembler
            .lock()
            .expect("conversation timeline mutex poisoned")
            .ingest_segment(segment)
    }

    pub fn ingest_capture_event(&self, event: &AudioCaptureEvent) -> Vec<TurnEvent> {
        let AudioCaptureEvent::Stopped { source } = event else {
            return Vec::new();
        };
        self.assembler
            .lock()
            .expect("conversation timeline mutex poisoned")
            .finalize_source(*source, FinalizationReason::Paused)
    }

    pub fn flush(&self) -> Vec<TurnEvent> {
        self.assembler
            .lock()
            .expect("conversation timeline mutex poisoned")
            .flush()
    }

    pub fn end_session(&self) -> Vec<TurnEvent> {
        self.assembler
            .lock()
            .expect("conversation timeline mutex poisoned")
            .end_session()
    }

    pub fn snapshot(&self) -> Vec<ConversationTurn> {
        self.assembler
            .lock()
            .expect("conversation timeline mutex poisoned")
            .snapshot()
    }

    pub fn raw_segments(&self) -> Vec<TranscriptSegment> {
        self.assembler
            .lock()
            .expect("conversation timeline mutex poisoned")
            .raw_segments()
    }
}

pub struct ConversationTimelineState(pub Arc<ConversationTimeline>);

#[tauri::command]
pub async fn conversation_timeline_snapshot_command(
    state: State<'_, ConversationTimelineState>,
) -> Result<Vec<ConversationTurn>, String> {
    Ok(state.0.snapshot())
}

#[tauri::command]
pub async fn conversation_flush_turns_command(
    app: AppHandle,
    state: State<'_, ConversationTimelineState>,
) -> Result<(), String> {
    emit_turn_events(&app, state.0.flush());
    Ok(())
}

#[tauri::command]
pub async fn conversation_end_session_command(
    app: AppHandle,
    state: State<'_, ConversationTimelineState>,
) -> Result<(), String> {
    emit_turn_events(&app, state.0.end_session());
    Ok(())
}

#[tauri::command]
pub async fn conversation_raw_segments_command(
    state: State<'_, ConversationTimelineState>,
) -> Result<Vec<TranscriptSegment>, String> {
    Ok(state.0.raw_segments())
}

pub fn emit_turn_events(app: &AppHandle, events: Vec<TurnEvent>) {
    for event in events {
        if let Err(e) = app.emit(CONVERSATION_TIMELINE_EVENT, &event) {
            warn!(%e, "failed to emit conversation timeline event to frontend");
        }
    }
}

fn normalize_segment_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn join_turn_text(current: &str, next: &str) -> String {
    let current = normalize_segment_text(current);
    let next = normalize_segment_text(next);
    if current.is_empty() {
        next
    } else if next.is_empty() {
        current
    } else {
        format!("{current} {next}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transcript(source: AudioSource, text: &str, start: u64, end: u64) -> Transcript {
        Transcript {
            segment_id: SegmentId::next(),
            source,
            text: text.into(),
            language: Some("pt".into()),
            started_at: AudioTimestamp(start),
            ended_at: AudioTimestamp(end),
            processing_time_ms: 12,
        }
    }

    fn assembler() -> TurnAssembler {
        TurnAssembler::new(TurnAssemblerConfig::default())
    }

    fn segment(source: AudioSource, text: &str, start: u64, end: u64) -> TranscriptSegment {
        TranscriptSegment::from_transcript(transcript(source, text, start, end), 1).unwrap()
    }

    fn finalized(events: &[TurnEvent]) -> Vec<&ConversationTurn> {
        events
            .iter()
            .filter_map(|event| match event {
                TurnEvent::Finalized { turn } => Some(turn),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn maps_sources_to_conversation_speakers() {
        assert_eq!(
            ConversationSpeaker::from(AudioSource::Microphone),
            ConversationSpeaker::User
        );
        assert_eq!(
            ConversationSpeaker::from(AudioSource::SystemOutput),
            ConversationSpeaker::OtherPerson
        );
    }

    #[test]
    fn two_consecutive_segments_from_same_speaker_form_one_turn() {
        let mut assembler = assembler();
        assembler.ingest_segment(segment(
            AudioSource::Microphone,
            "O Leandro tem 21 anos.",
            0,
            800,
        ));
        assembler.ingest_segment(segment(
            AudioSource::Microphone,
            "No Grupo Shop Mix...",
            1_200,
            2_000,
        ));

        let snapshot = assembler.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(
            snapshot[0].text,
            "O Leandro tem 21 anos. No Grupo Shop Mix..."
        );
        assert_eq!(snapshot[0].segments.len(), 2);
    }

    #[test]
    fn different_speakers_form_separate_turns() {
        let mut assembler = assembler();
        assembler.ingest_segment(segment(AudioSource::Microphone, "Olá.", 0, 500));
        let events =
            assembler.ingest_segment(segment(AudioSource::SystemOutput, "Tudo bem?", 600, 900));

        assert_eq!(finalized(&events).len(), 1);
        let snapshot = assembler.snapshot();
        assert_eq!(snapshot.len(), 2);
        assert_ne!(snapshot[0].speaker, snapshot[1].speaker);
    }

    #[test]
    fn gap_below_limit_merges() {
        let mut assembler = assembler();
        assembler.ingest_segment(segment(AudioSource::Microphone, "Parte um.", 0, 500));
        assembler.ingest_segment(segment(
            AudioSource::Microphone,
            "Parte dois.",
            2_299,
            2_700,
        ));

        assert_eq!(assembler.snapshot().len(), 1);
    }

    #[test]
    fn merge_gap_is_configurable() {
        let mut assembler = TurnAssembler::new(TurnAssemblerConfig {
            same_speaker_merge_gap_ms: 100,
            ..TurnAssemblerConfig::default()
        });
        assembler.ingest_segment(segment(AudioSource::Microphone, "Parte um.", 0, 500));
        assembler.ingest_segment(segment(AudioSource::Microphone, "Parte dois.", 700, 900));

        assert_eq!(assembler.snapshot().len(), 2);
    }

    #[test]
    fn gap_above_limit_separates() {
        let mut assembler = assembler();
        assembler.ingest_segment(segment(AudioSource::Microphone, "Parte um.", 0, 500));
        let events = assembler.ingest_segment(segment(
            AudioSource::Microphone,
            "Parte dois.",
            2_301,
            2_700,
        ));

        assert_eq!(finalized(&events).len(), 1);
        assert_eq!(assembler.snapshot().len(), 2);
    }

    #[test]
    fn source_change_separates_even_when_speaker_mapping_would_differ_anyway() {
        let mut assembler = assembler();
        assembler.ingest_segment(segment(AudioSource::Microphone, "Entrada.", 0, 500));
        let events =
            assembler.ingest_segment(segment(AudioSource::SystemOutput, "Saída.", 600, 900));

        assert_eq!(finalized(&events).len(), 1);
        assert_eq!(assembler.snapshot().len(), 2);
    }

    #[test]
    fn flush_finalizes_open_turn() {
        let mut assembler = assembler();
        assembler.ingest_segment(segment(AudioSource::Microphone, "Aberto.", 0, 500));

        let events = assembler.flush();

        assert_eq!(finalized(&events).len(), 1);
        assert!(assembler.snapshot()[0].finalized_at.is_some());
    }

    #[test]
    fn pause_finalizes_open_turn_for_that_source() {
        let mut assembler = assembler();
        assembler.ingest_segment(segment(AudioSource::Microphone, "Aberto.", 0, 500));

        let events = assembler.finalize_source(AudioSource::Microphone, FinalizationReason::Paused);

        assert_eq!(finalized(&events).len(), 1);
    }

    #[test]
    fn session_end_finalizes_open_turn_for_that_source() {
        let mut assembler = assembler();
        assembler.ingest_segment(segment(AudioSource::SystemOutput, "Aberto.", 0, 500));

        let events =
            assembler.finalize_source(AudioSource::SystemOutput, FinalizationReason::SessionEnded);

        assert_eq!(finalized(&events).len(), 1);
    }

    #[test]
    fn maximum_duration_finalizes_before_appending_over_limit() {
        let mut assembler = TurnAssembler::new(TurnAssemblerConfig {
            maximum_turn_duration_ms: 1_000,
            ..TurnAssemblerConfig::default()
        });
        assembler.ingest_segment(segment(AudioSource::Microphone, "Um.", 0, 700));
        let events =
            assembler.ingest_segment(segment(AudioSource::Microphone, "Dois.", 800, 1_200));

        assert_eq!(finalized(&events).len(), 1);
        assert_eq!(assembler.snapshot().len(), 2);
    }

    #[test]
    fn text_join_removes_duplicate_spaces_and_preserves_punctuation() {
        let mut assembler = assembler();
        assembler.ingest_segment(segment(
            AudioSource::Microphone,
            "O Leandro   tem 21 anos.",
            0,
            500,
        ));
        assembler.ingest_segment(segment(
            AudioSource::Microphone,
            " No Grupo   Shop Mix...",
            600,
            900,
        ));

        assert_eq!(
            assembler.snapshot()[0].text,
            "O Leandro tem 21 anos. No Grupo Shop Mix..."
        );
    }

    #[test]
    fn segment_ids_are_preserved() {
        let mut assembler = assembler();
        let first = segment(AudioSource::Microphone, "Um.", 0, 500);
        let second = segment(AudioSource::Microphone, "Dois.", 600, 900);
        let first_id = first.segment_id;
        let second_id = second.segment_id;

        assembler.ingest_segment(first);
        assembler.ingest_segment(second);

        assert_eq!(assembler.snapshot()[0].segments, vec![first_id, second_id]);
    }

    #[test]
    fn timestamps_cover_the_full_turn() {
        let mut assembler = assembler();
        assembler.ingest_segment(segment(AudioSource::Microphone, "Um.", 100, 500));
        assembler.ingest_segment(segment(AudioSource::Microphone, "Dois.", 600, 900));

        let turn = &assembler.snapshot()[0];
        assert_eq!(turn.started_at, AudioTimestamp(100));
        assert_eq!(turn.ended_at, AudioTimestamp(900));
    }

    #[test]
    fn emits_started_updated_and_finalized_events() {
        let mut assembler = assembler();
        let first_events =
            assembler.ingest_segment(segment(AudioSource::Microphone, "Um.", 0, 500));
        assert!(matches!(first_events[0], TurnEvent::Started { .. }));
        assert!(matches!(first_events[1], TurnEvent::Updated { .. }));

        let flush_events = assembler.flush();
        assert!(matches!(flush_events[0], TurnEvent::Finalized { .. }));
    }

    #[test]
    fn out_of_order_segment_is_tolerated_for_open_turn_without_mixing_speakers() {
        let mut assembler = assembler();
        assembler.ingest_segment(segment(AudioSource::Microphone, "Depois.", 1_000, 1_500));
        assembler.ingest_segment(segment(AudioSource::Microphone, "Antes.", 900, 1_100));

        let snapshot = assembler.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].speaker, ConversationSpeaker::User);
        assert_eq!(snapshot[0].segments.len(), 2);
    }

    #[test]
    fn out_of_order_tolerance_is_configurable() {
        let mut assembler = TurnAssembler::new(TurnAssemblerConfig {
            out_of_order_tolerance_ms: 100,
            ..TurnAssemblerConfig::default()
        });
        assembler.ingest_segment(segment(AudioSource::Microphone, "Depois.", 1_000, 1_500));
        assembler.ingest_segment(segment(
            AudioSource::Microphone,
            "Muito antes.",
            1_300,
            1_400,
        ));

        assert_eq!(assembler.snapshot().len(), 2);
    }

    #[test]
    fn out_of_order_different_speaker_never_mixes() {
        let mut assembler = assembler();
        assembler.ingest_segment(segment(AudioSource::Microphone, "Usuário.", 1_000, 1_500));
        assembler.ingest_segment(segment(AudioSource::SystemOutput, "Remoto.", 900, 1_100));

        let snapshot = assembler.snapshot();
        assert_eq!(snapshot.len(), 2);
        assert_ne!(snapshot[0].speaker, snapshot[1].speaker);
    }

    #[test]
    fn timeline_updates_the_same_open_turn_id() {
        let timeline = ConversationTimeline::default();
        let first = timeline.ingest_transcript_event(TranscriptEvent::Ready(transcript(
            AudioSource::Microphone,
            "Um.",
            0,
            500,
        )));
        let second = timeline.ingest_transcript_event(TranscriptEvent::Ready(transcript(
            AudioSource::Microphone,
            "Dois.",
            600,
            900,
        )));

        let first_turn_id = match &first[0] {
            TurnEvent::Started { turn_id, .. } => *turn_id,
            _ => panic!("expected started event"),
        };
        let second_turn_id = match &second[0] {
            TurnEvent::Updated { turn_id, .. } => *turn_id,
            _ => panic!("expected updated event"),
        };
        assert_eq!(first_turn_id, second_turn_id);
        assert_eq!(timeline.snapshot().len(), 1);
    }
}
