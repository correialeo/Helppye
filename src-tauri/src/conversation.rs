//! Conversation Timeline: an in-memory, ordered view of completed transcripts across
//! microphone and system-output sources. This is the first layer that treats both audio
//! streams as one conversation while preserving source, speaker role, timestamps, and a
//! deterministic receipt sequence for ties.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::State;

use crate::audio::segment::{AudioTimestamp, SegmentId};
use crate::audio::types::AudioSource;
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

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConversationTimelineItem {
    pub id: u64,
    pub segment_id: SegmentId,
    pub source: AudioSource,
    pub speaker: ConversationSpeaker,
    pub text: String,
    pub language: Option<String>,
    pub started_at: AudioTimestamp,
    pub ended_at: AudioTimestamp,
    pub processing_time_ms: u64,
    pub received_sequence: u64,
}

impl ConversationTimelineItem {
    fn from_transcript(transcript: Transcript, received_sequence: u64) -> Option<Self> {
        let text = transcript.text.trim().to_string();
        if text.is_empty() {
            return None;
        }

        Some(ConversationTimelineItem {
            id: received_sequence,
            segment_id: transcript.segment_id,
            source: transcript.source,
            speaker: ConversationSpeaker::from(transcript.source),
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
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConversationTimelineEvent {
    ItemAdded { item: ConversationTimelineItem },
}

#[derive(Default)]
pub struct ConversationTimeline {
    items: Mutex<Vec<ConversationTimelineItem>>,
    next_sequence: AtomicU64,
}

impl ConversationTimeline {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn ingest_transcript_event(
        &self,
        event: TranscriptEvent,
    ) -> Option<ConversationTimelineEvent> {
        let TranscriptEvent::Ready(transcript) = event else {
            return None;
        };

        let sequence = self.next_sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let item = ConversationTimelineItem::from_transcript(transcript, sequence)?;
        self.items
            .lock()
            .expect("conversation timeline mutex poisoned")
            .push(item.clone());
        Some(ConversationTimelineEvent::ItemAdded { item })
    }

    pub fn snapshot(&self) -> Vec<ConversationTimelineItem> {
        let mut items = self
            .items
            .lock()
            .expect("conversation timeline mutex poisoned")
            .clone();
        items.sort_by_key(|item| (item.started_at, item.ended_at, item.received_sequence));
        items
    }
}

pub struct ConversationTimelineState(pub Arc<ConversationTimeline>);

#[tauri::command]
pub async fn conversation_timeline_snapshot_command(
    state: State<'_, ConversationTimelineState>,
) -> Result<Vec<ConversationTimelineItem>, String> {
    Ok(state.0.snapshot())
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
    fn appends_ready_transcripts_and_sorts_snapshot_by_timeline_time() {
        let timeline = ConversationTimeline::new();

        timeline.ingest_transcript_event(TranscriptEvent::Ready(transcript(
            AudioSource::SystemOutput,
            "segunda fala",
            2_000,
            2_400,
        )));
        timeline.ingest_transcript_event(TranscriptEvent::Ready(transcript(
            AudioSource::Microphone,
            "primeira fala",
            1_000,
            1_300,
        )));

        let snapshot = timeline.snapshot();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot[0].text, "primeira fala");
        assert_eq!(snapshot[0].speaker, ConversationSpeaker::User);
        assert_eq!(snapshot[1].text, "segunda fala");
        assert_eq!(snapshot[1].speaker, ConversationSpeaker::OtherPerson);
    }

    #[test]
    fn ignores_failed_and_empty_transcripts() {
        let timeline = ConversationTimeline::new();

        timeline.ingest_transcript_event(TranscriptEvent::Failed {
            segment_id: SegmentId::next(),
            source: AudioSource::Microphone,
            message: "fake failure".into(),
        });
        timeline.ingest_transcript_event(TranscriptEvent::Ready(transcript(
            AudioSource::Microphone,
            "   ",
            0,
            100,
        )));

        assert!(timeline.snapshot().is_empty());
    }
}
