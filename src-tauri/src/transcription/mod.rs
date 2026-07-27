//! Local speech-to-text layer, decoupled from audio capture and segmentation.
//!
//! `provider::TranscriptionProvider` is the extension point for backends (see
//! `docs/local-transcription.md` for the evaluation behind the one implemented so far).
//! Consumes `audio::segment::AudioSegment`, produces `types::Transcript` — never touches
//! raw `AudioFrame`s or the capture pipeline directly.

pub mod error;
pub mod provider;
pub mod types;
pub mod whisper_provider;
