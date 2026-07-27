//! Local speech-to-text layer, decoupled from audio capture and segmentation.
//!
//! `provider::TranscriptionProvider` is the extension point for backends (see
//! `docs/local-transcription.md` for the evaluation behind the one implemented so far).
//! Consumes `audio::segment::AudioSegment`, produces `types::Transcript` — never touches
//! raw `AudioFrame`s or the capture pipeline directly. `queue::TranscriptionQueue` is the
//! bounded, non-blocking handoff point between segmentation and this layer.

pub mod error;
pub mod provider;
pub mod queue;
pub mod types;
pub mod whisper_provider;

use std::sync::Arc;

use serde::Serialize;
use tauri::State;
use tracing::info;

use provider::TranscriptionProvider;
use queue::TranscriptionQueue;
use types::{InferenceDevice, ModelConfig, TranscriptionLanguage};

/// Tauri event name for `types::TranscriptEvent`, mirroring `audio::CAPTURE_EVENT`.
pub const TRANSCRIPTION_EVENT: &str = "transcription://event";

/// Managed Tauri state holding the one configured `TranscriptionProvider` instance, the
/// shared transcription queue (same `Arc` handed to `audio::AudioState`), and the name of
/// whatever model was last successfully loaded (`None` until `configure_transcription_command`
/// succeeds at least once).
pub struct TranscriptionState {
    pub provider: Arc<dyn TranscriptionProvider>,
    pub queue: Arc<TranscriptionQueue>,
    loaded_model_name: tokio::sync::Mutex<Option<String>>,
}

impl TranscriptionState {
    pub fn new(provider: Arc<dyn TranscriptionProvider>, queue: Arc<TranscriptionQueue>) -> Self {
        TranscriptionState {
            provider,
            queue,
            loaded_model_name: tokio::sync::Mutex::new(None),
        }
    }
}

/// Loads (or reloads) the transcription model. No default `model_path` — an empty/missing
/// path is a configuration error, never a trigger to fetch a model automatically. `language`
/// of `None` or empty selects `TranscriptionLanguage::Automatic`; otherwise it's fixed to the
/// given tag (e.g. `"pt"`), the default the frontend offers.
#[tauri::command]
pub async fn configure_transcription_command(
    state: State<'_, TranscriptionState>,
    model_path: String,
    model_name: String,
    language: Option<String>,
) -> Result<(), String> {
    let language = match language {
        Some(tag) if !tag.is_empty() => TranscriptionLanguage::Fixed(tag),
        _ => TranscriptionLanguage::Automatic,
    };
    let config = ModelConfig {
        model_path: model_path.into(),
        model_name,
        language,
        device: InferenceDevice::Cpu,
    };
    let model_name = config.model_name.clone();
    state
        .provider
        .load(config)
        .await
        .map_err(|e| e.to_string())?;
    info!(
        provider = state.provider.provider_name(),
        model_name, "transcription model loaded"
    );
    *state.loaded_model_name.lock().await = Some(model_name);
    Ok(())
}

/// Snapshot of transcription-layer health for a dev-mode diagnostics panel — never affects
/// capture or transcription behavior, purely informational.
#[derive(Debug, Clone, Serialize)]
pub struct TranscriptionDiagnostics {
    pub provider_name: &'static str,
    pub loaded_model_name: Option<String>,
    pub dropped_segments: u64,
}

#[tauri::command]
pub async fn transcription_diagnostics_command(
    state: State<'_, TranscriptionState>,
) -> Result<TranscriptionDiagnostics, String> {
    Ok(TranscriptionDiagnostics {
        provider_name: state.provider.provider_name(),
        loaded_model_name: state.loaded_model_name.lock().await.clone(),
        dropped_segments: state.queue.dropped_segments(),
    })
}
