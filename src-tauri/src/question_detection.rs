//! Local question detection over assembled conversation turns.
//!
//! This phase intentionally stays rule-based. Interview prompts and indirect requests
//! are treated as `QuestionDetection` because they require the user to answer, even when
//! the transcript has no question mark.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use thiserror::Error;
use tracing::{debug, info};

use crate::audio::segment::AudioTimestamp;
use crate::audio::types::AudioSource;
use crate::conversation::{
    ConversationSpeaker, ConversationTimelineEvent, ConversationTurn, TurnId, UtteranceId,
};

pub const QUESTION_DETECTION_EVENT: &str = "question://detection-event";

const QUESTION_THRESHOLD: f32 = 0.60;
const HIGH_CONFIDENCE_THRESHOLD: f32 = 0.85;
const QUESTION_DETECTION_DEBOUNCE_MS: u64 = 800;

static NEXT_DETECTION_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct QuestionDetectionId(u64);

impl QuestionDetectionId {
    fn next() -> Self {
        QuestionDetectionId(NEXT_DETECTION_ID.fetch_add(1, Ordering::Relaxed))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionDetectionMode {
    RuleBased,
    Semantic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionDetectionStatus {
    Candidate,
    Confirmed,
    Dismissed,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QuestionSignal {
    QuestionMark {
        weight: f32,
    },
    InterrogativePrefix {
        phrase: &'static str,
        weight: f32,
    },
    InterrogativeConstruction {
        phrase: &'static str,
        weight: f32,
    },
    InterviewPattern {
        phrase: &'static str,
        weight: f32,
    },
    DirectedVerb {
        phrase: &'static str,
        weight: f32,
    },
    TranscriptFuzzyMatch {
        phrase: &'static str,
        observed: String,
        weight: f32,
    },
    Penalty {
        reason: &'static str,
        weight: f32,
    },
}

impl QuestionSignal {
    fn weight(&self) -> f32 {
        match self {
            QuestionSignal::QuestionMark { weight }
            | QuestionSignal::InterrogativePrefix { weight, .. }
            | QuestionSignal::InterrogativeConstruction { weight, .. }
            | QuestionSignal::InterviewPattern { weight, .. }
            | QuestionSignal::DirectedVerb { weight, .. }
            | QuestionSignal::TranscriptFuzzyMatch { weight, .. }
            | QuestionSignal::Penalty { weight, .. } => *weight,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QuestionDetection {
    pub id: QuestionDetectionId,
    pub turn_id: TurnId,
    pub speaker: ConversationSpeaker,
    pub source: AudioSource,
    pub detected: bool,
    pub confidence: f32,
    pub question_text: Option<String>,
    pub matched_signals: Vec<QuestionSignal>,
    pub detected_at: AudioTimestamp,
    pub detection_mode: QuestionDetectionMode,
    pub status: QuestionDetectionStatus,
    pub normalized_text: String,
    pub utterance_ids: Vec<UtteranceId>,
    pub status_reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum QuestionDetectionEvent {
    Candidate {
        detection: QuestionDetection,
    },
    Updated {
        detection: QuestionDetection,
    },
    Confirmed {
        detection: QuestionDetection,
    },
    Dismissed {
        detection_id: QuestionDetectionId,
        turn_id: TurnId,
    },
}

#[derive(Debug, Error)]
#[error("question detector failed: {message}")]
pub struct QuestionDetectionError {
    message: String,
}

#[async_trait]
pub trait QuestionDetector: Send + Sync {
    async fn detect(
        &self,
        turn: &ConversationTurn,
        context: &[ConversationTurn],
    ) -> Result<QuestionDetection, QuestionDetectionError>;

    fn provider_name(&self) -> &'static str;
}

#[derive(Debug, Clone, Default)]
pub struct RuleBasedQuestionDetector {
    config: RuleBasedQuestionDetectorConfig,
}

#[derive(Debug, Clone)]
pub struct RuleBasedQuestionDetectorConfig {
    pub question_threshold: f32,
    pub high_confidence_threshold: f32,
}

impl Default for RuleBasedQuestionDetectorConfig {
    fn default() -> Self {
        RuleBasedQuestionDetectorConfig {
            question_threshold: QUESTION_THRESHOLD,
            high_confidence_threshold: HIGH_CONFIDENCE_THRESHOLD,
        }
    }
}

#[async_trait]
impl QuestionDetector for RuleBasedQuestionDetector {
    async fn detect(
        &self,
        turn: &ConversationTurn,
        _context: &[ConversationTurn],
    ) -> Result<QuestionDetection, QuestionDetectionError> {
        let analysis = self.analyze(turn);
        let detected = analysis.confidence >= self.config.question_threshold;
        Ok(QuestionDetection {
            id: QuestionDetectionId::next(),
            turn_id: turn.id,
            speaker: turn.speaker,
            source: turn.source,
            detected,
            confidence: analysis.confidence,
            question_text: detected.then_some(analysis.question_text),
            matched_signals: analysis.signals,
            detected_at: turn.ended_at,
            detection_mode: QuestionDetectionMode::RuleBased,
            status: if detected {
                QuestionDetectionStatus::Candidate
            } else {
                QuestionDetectionStatus::Dismissed
            },
            normalized_text: analysis.normalized_text,
            utterance_ids: analysis.utterance_ids,
            status_reason: if analysis.confidence >= self.config.high_confidence_threshold {
                "high_confidence_score_above_threshold".into()
            } else if detected {
                "score_above_threshold".into()
            } else {
                "score_below_threshold".into()
            },
        })
    }

    fn provider_name(&self) -> &'static str {
        "rule_based"
    }
}

impl RuleBasedQuestionDetector {
    pub fn analyze(&self, turn: &ConversationTurn) -> RuleBasedQuestionAnalysis {
        if !is_eligible_turn(turn) {
            return RuleBasedQuestionAnalysis::empty(turn, "ineligible_source_or_speaker");
        }

        let normalized_full = normalize_question_text(&turn.text);
        if normalized_full.is_empty() {
            return RuleBasedQuestionAnalysis::empty(turn, "empty_text");
        }

        let candidate = extract_question_candidate(&turn.text);
        let normalized = normalize_question_text(&candidate);
        if normalized.is_empty() {
            return RuleBasedQuestionAnalysis::empty(turn, "empty_candidate");
        }

        let mut signals = Vec::new();
        if candidate.trim_end().ends_with('?') || turn.text.trim_end().ends_with('?') {
            signals.push(QuestionSignal::QuestionMark { weight: 0.45 });
        }

        if let Some(phrase) = find_prefix(&normalized, INTERROGATIVE_PREFIXES) {
            signals.push(QuestionSignal::InterrogativePrefix {
                phrase,
                weight: 0.35,
            });
        }

        if let Some(phrase) = find_contained_phrase(&normalized, INTERROGATIVE_CONSTRUCTIONS) {
            signals.push(QuestionSignal::InterrogativeConstruction {
                phrase,
                weight: 0.30,
            });
        }

        if let Some(phrase) = find_contained_phrase(&normalized, INTERVIEW_PATTERNS) {
            signals.push(QuestionSignal::InterviewPattern {
                phrase,
                weight: 0.40,
            });
        }

        if let Some(phrase) = find_contained_phrase(&normalized, DIRECTED_VERBS) {
            signals.push(QuestionSignal::DirectedVerb {
                phrase,
                weight: 0.30,
            });
        }

        if let Some((phrase, observed)) = find_fuzzy_transcription_match(&normalized) {
            signals.push(QuestionSignal::TranscriptFuzzyMatch {
                phrase,
                observed,
                weight: 0.30,
            });
        }

        if starts_with_non_question_discourse(&normalized) {
            signals.push(QuestionSignal::Penalty {
                reason: "discourse_marker_not_question",
                weight: -0.45,
            });
        }

        if looks_like_embedded_clause(&normalized) {
            signals.push(QuestionSignal::Penalty {
                reason: "embedded_interrogative_clause",
                weight: -0.35,
            });
        }

        let word_count = normalized.split_whitespace().count();
        if word_count <= 2 {
            signals.push(QuestionSignal::Penalty {
                reason: "too_short_or_incomplete",
                weight: -0.20,
            });
        }
        if is_standalone_interrogative(&normalized) {
            signals.push(QuestionSignal::Penalty {
                reason: "standalone_interrogative_word",
                weight: -0.20,
            });
        }

        let confidence = clamp_confidence(signals.iter().map(QuestionSignal::weight).sum());
        let question_text = restore_candidate_text(&candidate);
        RuleBasedQuestionAnalysis {
            confidence,
            question_text,
            normalized_text: normalized,
            signals,
            utterance_ids: candidate_utterance_ids(turn),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuleBasedQuestionAnalysis {
    pub confidence: f32,
    pub question_text: String,
    pub normalized_text: String,
    pub signals: Vec<QuestionSignal>,
    pub utterance_ids: Vec<UtteranceId>,
}

impl RuleBasedQuestionAnalysis {
    fn empty(turn: &ConversationTurn, reason: &'static str) -> Self {
        RuleBasedQuestionAnalysis {
            confidence: 0.0,
            question_text: String::new(),
            normalized_text: String::new(),
            signals: vec![QuestionSignal::Penalty {
                reason,
                weight: 0.0,
            }],
            utterance_ids: candidate_utterance_ids(turn),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct QuestionDetectionProcessorConfig {
    pub debounce_ms: u64,
    pub question_threshold: f32,
}

impl Default for QuestionDetectionProcessorConfig {
    fn default() -> Self {
        QuestionDetectionProcessorConfig {
            debounce_ms: QUESTION_DETECTION_DEBOUNCE_MS,
            question_threshold: QUESTION_THRESHOLD,
        }
    }
}

#[derive(Debug, Clone)]
struct DetectionRecord {
    detection: QuestionDetection,
    fingerprint: u64,
    status: QuestionDetectionStatus,
    last_changed_at_ms: u64,
    last_confirmed_fingerprint: Option<u64>,
}

#[derive(Debug)]
pub struct QuestionDetectionProcessor {
    config: QuestionDetectionProcessorConfig,
    turns: HashMap<TurnId, ConversationTurn>,
    detections_by_turn: HashMap<TurnId, DetectionRecord>,
}

impl Default for QuestionDetectionProcessor {
    fn default() -> Self {
        Self::new(QuestionDetectionProcessorConfig::default())
    }
}

impl QuestionDetectionProcessor {
    pub fn new(config: QuestionDetectionProcessorConfig) -> Self {
        QuestionDetectionProcessor {
            config,
            turns: HashMap::new(),
            detections_by_turn: HashMap::new(),
        }
    }

    pub fn apply_turn_detection(
        &mut self,
        mut detection: QuestionDetection,
        turn: ConversationTurn,
        is_finalized: bool,
        now_ms: u64,
    ) -> Vec<QuestionDetectionEvent> {
        self.turns.insert(turn.id, turn);
        if !detection.detected || detection.confidence < self.config.question_threshold {
            return self.dismiss_existing(detection.turn_id, now_ms, "score_below_threshold");
        }

        detection.status = QuestionDetectionStatus::Candidate;
        let fingerprint = fingerprint_question(detection.normalized_text.as_str());
        let turn_id = detection.turn_id;
        if let Some(record) = self.detections_by_turn.get_mut(&turn_id) {
            let changed = record.fingerprint != fingerprint
                || record.status == QuestionDetectionStatus::Dismissed;
            detection.id = record.detection.id;
            if changed {
                record.fingerprint = fingerprint;
                record.last_changed_at_ms = now_ms;
                record.status = QuestionDetectionStatus::Candidate;
                record.detection = detection;
                debug!(
                    turn_id = turn_id.value(),
                    confidence = record.detection.confidence,
                    signals = ?record.detection.matched_signals,
                    normalized_text = %record.detection.normalized_text,
                    "question candidate detected"
                );
                return vec![QuestionDetectionEvent::Updated {
                    detection: record.detection.clone(),
                }];
            }
            record.detection = detection;
            if is_finalized
                || now_ms.saturating_sub(record.last_changed_at_ms) >= self.config.debounce_ms
            {
                return confirm_record(record, turn_id);
            }
            return Vec::new();
        }

        debug!(
            turn_id = turn_id.value(),
            confidence = detection.confidence,
            signals = ?detection.matched_signals,
            normalized_text = %detection.normalized_text,
            "question candidate detected"
        );
        let record = DetectionRecord {
            detection,
            fingerprint,
            status: QuestionDetectionStatus::Candidate,
            last_changed_at_ms: now_ms,
            last_confirmed_fingerprint: None,
        };
        let event = QuestionDetectionEvent::Candidate {
            detection: record.detection.clone(),
        };
        self.detections_by_turn.insert(turn_id, record);
        vec![event]
    }

    pub fn confirm_due(&mut self, now_ms: u64) -> Vec<QuestionDetectionEvent> {
        let mut events = Vec::new();
        for (turn_id, record) in &mut self.detections_by_turn {
            if record.status != QuestionDetectionStatus::Candidate {
                continue;
            }
            if now_ms.saturating_sub(record.last_changed_at_ms) >= self.config.debounce_ms {
                events.extend(confirm_record(record, *turn_id));
            }
        }
        events
    }

    pub fn mark_turn_as_question(
        &mut self,
        turn_id: TurnId,
        now_ms: u64,
    ) -> Vec<QuestionDetectionEvent> {
        let Some(turn) = self.turns.get(&turn_id).cloned() else {
            return Vec::new();
        };
        let normalized_text = normalize_question_text(&turn.text);
        let detection = QuestionDetection {
            id: self
                .detections_by_turn
                .get(&turn_id)
                .map(|record| record.detection.id)
                .unwrap_or_else(QuestionDetectionId::next),
            turn_id,
            speaker: turn.speaker,
            source: turn.source,
            detected: true,
            confidence: 1.0,
            question_text: Some(restore_candidate_text(&turn.text)),
            matched_signals: vec![QuestionSignal::DirectedVerb {
                phrase: "manual",
                weight: 1.0,
            }],
            detected_at: turn.ended_at,
            detection_mode: QuestionDetectionMode::RuleBased,
            status: QuestionDetectionStatus::Confirmed,
            normalized_text: normalized_text.clone(),
            utterance_ids: turn.utterances.clone(),
            status_reason: "manual_mark".into(),
        };
        let fingerprint = fingerprint_question(&normalized_text);
        self.detections_by_turn.insert(
            turn_id,
            DetectionRecord {
                detection: detection.clone(),
                fingerprint,
                status: QuestionDetectionStatus::Confirmed,
                last_changed_at_ms: now_ms,
                last_confirmed_fingerprint: Some(fingerprint),
            },
        );
        vec![QuestionDetectionEvent::Confirmed { detection }]
    }

    pub fn dismiss_turn_question(&mut self, turn_id: TurnId) -> Vec<QuestionDetectionEvent> {
        self.dismiss_existing(turn_id, 0, "manual_dismiss")
    }

    fn dismiss_existing(
        &mut self,
        turn_id: TurnId,
        now_ms: u64,
        reason: &'static str,
    ) -> Vec<QuestionDetectionEvent> {
        let Some(record) = self.detections_by_turn.get_mut(&turn_id) else {
            return Vec::new();
        };
        if record.status == QuestionDetectionStatus::Dismissed {
            return Vec::new();
        }
        record.status = QuestionDetectionStatus::Dismissed;
        record.last_changed_at_ms = now_ms;
        record.detection.status = QuestionDetectionStatus::Dismissed;
        record.detection.status_reason = reason.into();
        info!(
            turn_id = turn_id.value(),
            reason, "question candidate dismissed"
        );
        vec![QuestionDetectionEvent::Dismissed {
            detection_id: record.detection.id,
            turn_id,
        }]
    }
}

fn confirm_record(record: &mut DetectionRecord, turn_id: TurnId) -> Vec<QuestionDetectionEvent> {
    if record.last_confirmed_fingerprint == Some(record.fingerprint) {
        return Vec::new();
    }
    record.status = QuestionDetectionStatus::Confirmed;
    record.last_confirmed_fingerprint = Some(record.fingerprint);
    record.detection.status = QuestionDetectionStatus::Confirmed;
    record.detection.status_reason = "debounce_elapsed".into();
    info!(
        turn_id = turn_id.value(),
        confidence = record.detection.confidence,
        "question confirmed"
    );
    debug!(
        turn_id = turn_id.value(),
        question = ?record.detection.question_text,
        confidence = record.detection.confidence,
        signals = ?record.detection.matched_signals,
        "question confirmed details"
    );
    vec![QuestionDetectionEvent::Confirmed {
        detection: record.detection.clone(),
    }]
}

pub struct QuestionDetectionState {
    detector: Arc<dyn QuestionDetector>,
    processor: Mutex<QuestionDetectionProcessor>,
    started_at: Instant,
}

impl Default for QuestionDetectionState {
    fn default() -> Self {
        let _semantic_mode_marker = QuestionDetectionMode::Semantic;
        QuestionDetectionState {
            detector: Arc::new(RuleBasedQuestionDetector::default()),
            processor: Mutex::new(QuestionDetectionProcessor::default()),
            started_at: Instant::now(),
        }
    }
}

impl QuestionDetectionState {
    fn now_ms(&self) -> u64 {
        self.started_at
            .elapsed()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64
    }
}

pub fn process_conversation_events(
    app: &AppHandle,
    state: Arc<QuestionDetectionState>,
    events: &[ConversationTimelineEvent],
) {
    for event in events {
        let (turn, is_finalized) = match event {
            ConversationTimelineEvent::TurnUpdated { turn } => (turn.clone(), false),
            ConversationTimelineEvent::TurnFinalized { turn } => (turn.clone(), true),
            _ => continue,
        };
        let app_handle = app.clone();
        let state_for_task = state.clone();
        tauri::async_runtime::spawn(async move {
            let context = {
                state_for_task
                    .processor
                    .lock()
                    .expect("question detection mutex poisoned")
                    .turns
                    .values()
                    .cloned()
                    .collect::<Vec<_>>()
            };
            let detection = match state_for_task.detector.detect(&turn, &context).await {
                Ok(detection) => detection,
                Err(e) => {
                    tracing::warn!(
                        provider = state_for_task.detector.provider_name(),
                        %e,
                        "question detector failed"
                    );
                    return;
                }
            };
            let now_ms = state_for_task.now_ms();
            let events = state_for_task
                .processor
                .lock()
                .expect("question detection mutex poisoned")
                .apply_turn_detection(detection, turn, is_finalized, now_ms);
            emit_question_detection_events(&app_handle, events);
            schedule_debounce_confirmation(&app_handle, state_for_task);
        });
    }
}

fn schedule_debounce_confirmation(app: &AppHandle, state: Arc<QuestionDetectionState>) {
    let app_handle = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(QUESTION_DETECTION_DEBOUNCE_MS)).await;
        let now_ms = state.now_ms();
        let events = state
            .processor
            .lock()
            .expect("question detection mutex poisoned")
            .confirm_due(now_ms);
        emit_question_detection_events(&app_handle, events);
    });
}

#[tauri::command]
pub async fn question_mark_turn_as_question_command(
    app: AppHandle,
    state: State<'_, Arc<QuestionDetectionState>>,
    turn_id: u64,
) -> Result<(), String> {
    let events = state
        .processor
        .lock()
        .map_err(|_| "question detection mutex poisoned".to_string())?
        .mark_turn_as_question(TurnId::from_raw(turn_id), state.now_ms());
    emit_question_detection_events(&app, events);
    Ok(())
}

#[tauri::command]
pub async fn question_dismiss_turn_question_command(
    app: AppHandle,
    state: State<'_, Arc<QuestionDetectionState>>,
    turn_id: u64,
) -> Result<(), String> {
    let events = state
        .processor
        .lock()
        .map_err(|_| "question detection mutex poisoned".to_string())?
        .dismiss_turn_question(TurnId::from_raw(turn_id));
    emit_question_detection_events(&app, events);
    Ok(())
}

pub fn emit_question_detection_events(app: &AppHandle, events: Vec<QuestionDetectionEvent>) {
    for event in events {
        if let Err(e) = app.emit(QUESTION_DETECTION_EVENT, &event) {
            tracing::warn!(%e, "failed to emit question detection event to frontend");
        }
    }
}

fn is_eligible_turn(turn: &ConversationTurn) -> bool {
    turn.speaker == ConversationSpeaker::OtherPerson && turn.source == AudioSource::SystemOutput
}

fn normalize_question_text(text: &str) -> String {
    let without_noise = text
        .replace("[inaudível]", " ")
        .replace("[inaudivel]", " ")
        .replace("[silêncio]", " ")
        .replace("[silencio]", " ");
    without_noise
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn restore_candidate_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn extract_question_candidate(text: &str) -> String {
    let clean = restore_candidate_text(text);
    if clean.is_empty() {
        return clean;
    }
    if let Some(question_mark) = clean.rfind('?') {
        let prefix = &clean[..question_mark];
        let start = prefix
            .rfind(['.', '!', '\n'])
            .map(|idx| idx + 1)
            .unwrap_or(0);
        return clean[start..=question_mark].trim().to_string();
    }

    let parts = split_candidate_clauses(&clean);
    for index in (0..parts.len()).rev() {
        let tail = parts[index..].join(" ");
        let normalized = normalize_question_text(&tail);
        if has_strong_question_signal(&normalized) {
            return tail.trim().to_string();
        }
    }
    parts.last().cloned().unwrap_or(clean)
}

fn split_candidate_clauses(text: &str) -> Vec<String> {
    text.split(['.', '!', '\n'])
        .flat_map(|part| {
            let trimmed = part.trim();
            if trimmed.len() > 120 {
                trimmed.split(", ").map(str::trim).collect::<Vec<_>>()
            } else {
                vec![trimmed]
            }
        })
        .filter(|part| !part.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn has_strong_question_signal(normalized: &str) -> bool {
    find_prefix(normalized, INTERROGATIVE_PREFIXES).is_some()
        || find_contained_phrase(normalized, INTERROGATIVE_CONSTRUCTIONS).is_some()
        || find_contained_phrase(normalized, INTERVIEW_PATTERNS).is_some()
        || find_fuzzy_transcription_match(normalized).is_some()
}

fn find_prefix(text: &str, phrases: &[&'static str]) -> Option<&'static str> {
    phrases
        .iter()
        .copied()
        .find(|phrase| text == *phrase || text.starts_with(&format!("{phrase} ")))
}

fn find_contained_phrase(text: &str, phrases: &[&'static str]) -> Option<&'static str> {
    phrases
        .iter()
        .copied()
        .find(|phrase| contains_phrase(text, phrase))
}

fn contains_phrase(text: &str, phrase: &str) -> bool {
    text == phrase
        || text.starts_with(&format!("{phrase} "))
        || text.ends_with(&format!(" {phrase}"))
        || text.contains(&format!(" {phrase} "))
}

fn starts_with_non_question_discourse(text: &str) -> bool {
    NON_QUESTION_PREFIXES
        .iter()
        .any(|prefix| text.starts_with(prefix))
}

fn looks_like_embedded_clause(text: &str) -> bool {
    EMBEDDED_NON_QUESTION_PATTERNS
        .iter()
        .any(|phrase| contains_phrase(text, phrase))
}

fn is_standalone_interrogative(text: &str) -> bool {
    INTERROGATIVE_PREFIXES.contains(&text)
}

fn find_fuzzy_transcription_match(text: &str) -> Option<(&'static str, String)> {
    let words = text.split_whitespace().collect::<Vec<_>>();
    words.windows(2).find_map(|window| {
        let observed = format!("{} {}", window[0], window[1]);
        (levenshtein(&observed, "me descreva") <= 1 || levenshtein(&observed, "me descreve") <= 1)
            .then_some(("me descreva", observed))
    })
}

fn levenshtein(a: &str, b: &str) -> usize {
    let mut costs = (0..=b.chars().count()).collect::<Vec<_>>();
    for (i, ca) in a.chars().enumerate() {
        let mut last = i;
        costs[0] = i + 1;
        for (j, cb) in b.chars().enumerate() {
            let old = costs[j + 1];
            costs[j + 1] = if ca == cb {
                last
            } else {
                1 + last.min(old).min(costs[j])
            };
            last = old;
        }
    }
    costs[b.chars().count()]
}

fn candidate_utterance_ids(turn: &ConversationTurn) -> Vec<UtteranceId> {
    let count = turn.utterances.len().min(2);
    turn.utterances
        .iter()
        .skip(turn.utterances.len().saturating_sub(count))
        .copied()
        .collect()
}

fn clamp_confidence(value: f32) -> f32 {
    value.clamp(0.0, 1.0)
}

fn fingerprint_question(text: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

const INTERROGATIVE_PREFIXES: &[&str] = &[
    "qual", "quais", "quem", "quando", "onde", "como", "por que", "porque", "quanto", "quantos",
    "quantas", "o que",
];

const INTERROGATIVE_CONSTRUCTIONS: &[&str] = &[
    "o que",
    "o que você",
    "qual você",
    "como você",
    "por que você",
    "você pode",
    "você poderia",
    "poderia me",
    "consegue me",
    "me explique",
    "me explica",
    "me conta",
    "me conte",
    "me descreva",
    "me descreve",
    "fale sobre",
    "diga uma situação",
    "conte uma situação",
    "me dê um exemplo",
    "me de um exemplo",
    "qual foi",
    "como lidou",
    "o que faria",
    "o que você faria",
    "onde você",
    "quando você",
    "gostaria que você explicasse",
    "quero que você me conte",
    "pode falar",
];

const INTERVIEW_PATTERNS: &[&str] = &[
    "fale sobre você",
    "me conte sobre sua experiência",
    "qual foi seu maior desafio",
    "como você resolveu",
    "como você lidou",
    "me descreva uma situação",
    "me descreve uma situação",
    "por que devemos contratar você",
    "por que você quer trabalhar aqui",
    "onde você se vê",
    "quais são seus pontos fortes",
    "quais são seus pontos fracos",
    "por que saiu da empresa",
    "qual foi seu papel",
    "o que você aprendeu",
    "como prioriza tarefas",
    "como lida com conflitos",
    "como trabalha sob pressão",
    "desafio técnico mais difícil",
    "desafio tecnico mais dificil",
];

const DIRECTED_VERBS: &[&str] = &[
    "você pode",
    "você poderia",
    "poderia me",
    "consegue me",
    "me explique",
    "me explica",
    "me conta",
    "me conte",
    "me descreva",
    "me descreve",
    "fale sobre",
    "pode falar",
    "gostaria que você",
    "quero que você",
];

const NON_QUESTION_PREFIXES: &[&str] = &[
    "como disse",
    "como eu disse",
    "como falei",
    "como mencionado",
    "como expliquei",
];

const EMBEDDED_NON_QUESTION_PATTERNS: &[&str] = &[
    "eu não sei como",
    "eu nao sei como",
    "ele explicou por que",
    "ela explicou por que",
    "vamos ver quando",
    "não lembro onde",
    "nao lembro onde",
    "essa é uma pergunta",
    "essa e uma pergunta",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(source: AudioSource, speaker: ConversationSpeaker, text: &str) -> ConversationTurn {
        ConversationTurn {
            id: TurnId::from_raw(1),
            speaker,
            source,
            text: text.into(),
            utterances: vec![UtteranceId::from_raw(10), UtteranceId::from_raw(11)],
            started_at: AudioTimestamp(0),
            ended_at: AudioTimestamp(1_000),
            finalized_at: None,
        }
    }

    fn other(text: &str) -> ConversationTurn {
        turn(
            AudioSource::SystemOutput,
            ConversationSpeaker::OtherPerson,
            text,
        )
    }

    fn user(text: &str) -> ConversationTurn {
        turn(AudioSource::Microphone, ConversationSpeaker::User, text)
    }

    async fn detect(text: &str) -> QuestionDetection {
        RuleBasedQuestionDetector::default()
            .detect(&other(text), &[])
            .await
            .unwrap()
    }

    fn processor() -> QuestionDetectionProcessor {
        QuestionDetectionProcessor::new(QuestionDetectionProcessorConfig {
            debounce_ms: 800,
            question_threshold: 0.60,
        })
    }

    #[tokio::test]
    async fn detects_question_with_question_mark() {
        assert!(detect("Você pode explicar isso?").await.detected);
    }

    #[tokio::test]
    async fn detects_question_without_question_mark() {
        assert!(detect("você pode explicar isso").await.detected);
    }

    #[tokio::test]
    async fn detects_question_started_by_qual() {
        assert!(
            detect("qual você diria que foi o desafio técnico")
                .await
                .detected
        );
    }

    #[tokio::test]
    async fn detects_question_started_by_como() {
        assert!(
            detect("Como você resolveu no seu relacionamento?")
                .await
                .detected
        );
    }

    #[tokio::test]
    async fn como_disse_is_not_question() {
        assert!(
            !detect("Como disse, foi o meu primeiro trabalho.")
                .await
                .detected
        );
    }

    #[tokio::test]
    async fn por_que_voce_is_question() {
        assert!(detect("por que você quer trabalhar aqui").await.detected);
    }

    #[tokio::test]
    async fn ele_explicou_por_que_is_not_question() {
        assert!(!detect("ele explicou por que saiu").await.detected);
    }

    #[tokio::test]
    async fn me_descreva_uma_situacao_is_request() {
        assert!(
            detect("me descreva uma situação em que precisou aprender rápido")
                .await
                .detected
        );
    }

    #[tokio::test]
    async fn fale_sobre_voce_is_request() {
        assert!(detect("fale sobre você").await.detected);
    }

    #[tokio::test]
    async fn technical_interview_question_is_detected() {
        assert!(
            detect("qual foi o desafio técnico mais difícil que você resolveu")
                .await
                .detected
        );
    }

    #[tokio::test]
    async fn behavioral_question_is_detected() {
        assert!(detect("como lida com conflitos no trabalho").await.detected);
    }

    #[tokio::test]
    async fn declarative_sentence_is_not_detected() {
        assert!(!detect("Você fez isso ontem.").await.detected);
    }

    #[tokio::test]
    async fn question_at_end_of_long_turn_is_extracted() {
        let detection = detect("No nosso time usamos vários microsserviços e temos muitos deploys. Nesse contexto, qual foi o desafio técnico mais difícil que você resolveu?").await;
        assert_eq!(
            detection.question_text.as_deref(),
            Some("Nesse contexto, qual foi o desafio técnico mais difícil que você resolveu?")
        );
    }

    #[tokio::test]
    async fn two_utterances_can_compose_question() {
        let detection = detect("Nesse contexto. Qual foi seu maior desafio").await;
        assert!(detection.detected);
        assert_eq!(detection.utterance_ids.len(), 2);
    }

    #[tokio::test]
    async fn question_updated_during_turn_updated_emits_updated() {
        let mut p = processor();
        let first = detect("Qual foi seu maior desafio").await;
        assert!(matches!(
            p.apply_turn_detection(first, other("Qual foi seu maior desafio"), false, 0)[0],
            QuestionDetectionEvent::Candidate { .. }
        ));
        let updated = detect("Qual foi seu maior desafio e como você resolveu").await;
        assert!(matches!(
            p.apply_turn_detection(
                updated,
                other("Qual foi seu maior desafio e como você resolveu"),
                false,
                100
            )[0],
            QuestionDetectionEvent::Updated { .. }
        ));
    }

    #[tokio::test]
    async fn deduplicates_same_question() {
        let mut p = processor();
        let first = detect("Qual foi seu maior desafio").await;
        p.apply_turn_detection(first, other("Qual foi seu maior desafio"), false, 0);
        let second = detect("Qual foi seu maior desafio").await;
        assert!(p
            .apply_turn_detection(second, other("Qual foi seu maior desafio"), false, 100)
            .is_empty());
    }

    #[tokio::test]
    async fn candidate_becomes_confirmed_after_debounce() {
        let mut p = processor();
        let detection = detect("Qual foi seu maior desafio").await;
        p.apply_turn_detection(detection, other("Qual foi seu maior desafio"), false, 0);
        assert!(matches!(
            p.confirm_due(801)[0],
            QuestionDetectionEvent::Confirmed { .. }
        ));
    }

    #[tokio::test]
    async fn candidate_can_be_dismissed() {
        let mut p = processor();
        let detection = detect("Qual foi seu maior desafio").await;
        p.apply_turn_detection(detection, other("Qual foi seu maior desafio"), false, 0);
        assert!(matches!(
            p.dismiss_turn_question(TurnId::from_raw(1))[0],
            QuestionDetectionEvent::Dismissed { .. }
        ));
    }

    #[tokio::test]
    async fn only_system_output_is_eligible() {
        let detection = RuleBasedQuestionDetector::default()
            .detect(
                &turn(
                    AudioSource::Microphone,
                    ConversationSpeaker::OtherPerson,
                    "qual foi?",
                ),
                &[],
            )
            .await
            .unwrap();
        assert!(!detection.detected);
    }

    #[tokio::test]
    async fn user_question_does_not_trigger() {
        let detection = RuleBasedQuestionDetector::default()
            .detect(&user("qual foi?"), &[])
            .await
            .unwrap();
        assert!(!detection.detected);
    }

    #[tokio::test]
    async fn empty_text_is_not_question() {
        assert!(!detect("").await.detected);
    }

    #[tokio::test]
    async fn very_short_text_is_not_question() {
        assert!(!detect("como").await.detected);
    }

    #[tokio::test]
    async fn accents_are_supported() {
        assert!(detect("quais são seus pontos fortes").await.detected);
    }

    #[tokio::test]
    async fn missing_punctuation_is_supported() {
        assert!(
            detect("qual você diria que foi o desafio técnico mais difícil")
                .await
                .detected
        );
    }

    #[tokio::test]
    async fn light_transcription_error_is_supported() {
        assert!(
            detect("me descreve uma situação em que precisou aprender rápido")
                .await
                .detected
        );
    }

    #[tokio::test]
    async fn confidence_is_clamped() {
        let detection = detect("Você pode me explicar o que você faria?").await;
        assert!((0.0..=1.0).contains(&detection.confidence));
    }

    #[tokio::test]
    async fn extracts_candidate_question() {
        assert_eq!(
            extract_question_candidate("Explicação antes. Qual foi seu papel"),
            "Qual foi seu papel"
        );
    }

    #[tokio::test]
    async fn emits_candidate_event() {
        let mut p = processor();
        let detection = detect("Qual foi seu maior desafio").await;
        assert!(matches!(
            p.apply_turn_detection(detection, other("Qual foi seu maior desafio"), false, 0)[0],
            QuestionDetectionEvent::Candidate { .. }
        ));
    }

    #[tokio::test]
    async fn emits_updated_event() {
        let mut p = processor();
        let first = detect("Qual foi seu maior desafio").await;
        p.apply_turn_detection(first, other("Qual foi seu maior desafio"), false, 0);
        let updated = detect("Qual foi seu maior desafio e como você resolveu").await;
        assert!(matches!(
            p.apply_turn_detection(
                updated,
                other("Qual foi seu maior desafio e como você resolveu"),
                false,
                100
            )[0],
            QuestionDetectionEvent::Updated { .. }
        ));
    }

    #[tokio::test]
    async fn emits_confirmed_event() {
        let mut p = processor();
        let detection = detect("Qual foi seu maior desafio").await;
        p.apply_turn_detection(detection, other("Qual foi seu maior desafio"), false, 0);
        assert!(matches!(
            p.confirm_due(801)[0],
            QuestionDetectionEvent::Confirmed { .. }
        ));
    }

    #[tokio::test]
    async fn debounce_waits_before_confirmation() {
        let mut p = processor();
        let detection = detect("Qual foi seu maior desafio").await;
        p.apply_turn_detection(detection, other("Qual foi seu maior desafio"), false, 0);
        assert!(p.confirm_due(799).is_empty());
    }

    #[tokio::test]
    async fn flush_or_finalized_turn_confirms_question() {
        let mut p = processor();
        let detection = detect("Qual foi seu maior desafio").await;
        p.apply_turn_detection(detection, other("Qual foi seu maior desafio"), false, 0);
        let finalized = detect("Qual foi seu maior desafio").await;
        assert!(matches!(
            p.apply_turn_detection(finalized, other("Qual foi seu maior desafio"), true, 100)[0],
            QuestionDetectionEvent::Confirmed { .. }
        ));
    }

    #[tokio::test]
    async fn question_followed_by_explanation_keeps_question_candidate() {
        let detection =
            detect("Qual foi seu maior desafio? Pode responder com um exemplo específico.").await;
        assert_eq!(
            detection.question_text.as_deref(),
            Some("Qual foi seu maior desafio?")
        );
    }

    #[tokio::test]
    async fn explanation_followed_by_question_is_detected() {
        let detection = detect("Falamos sobre backend. Qual foi seu papel").await;
        assert_eq!(
            detection.question_text.as_deref(),
            Some("Qual foi seu papel")
        );
    }

    #[tokio::test]
    async fn does_not_emit_same_question_twice_after_confirmation() {
        let mut p = processor();
        let detection = detect("Qual foi seu maior desafio").await;
        p.apply_turn_detection(detection, other("Qual foi seu maior desafio"), false, 0);
        p.confirm_due(801);
        let same = detect("Qual foi seu maior desafio").await;
        assert!(p
            .apply_turn_detection(same, other("Qual foi seu maior desafio"), false, 900)
            .is_empty());
    }

    #[tokio::test]
    async fn real_validation_examples() {
        assert!(detect("Nesse contexto, qual você diria que foi o desafio técnico mais difícil que você já resolveu e como você lidou com ele?").await.detected);
        assert!(detect("Me descreve uma situação em que você precisou aprender algo rápido para entregar um resultado.").await.detected);
        assert!(
            detect("Como você resolveu no seu relacionamento?")
                .await
                .detected
        );
        assert!(
            !detect("Como disse, foi o meu primeiro trabalho.")
                .await
                .detected
        );
    }
}
