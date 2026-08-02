//! Persisted, non-sensitive transcription configuration.
//!
//! Provider-specific settings live under `providers`. Credentials deliberately do not:
//! they are stored only by `transcription::secrets` in the operating-system keychain.

use serde::{Deserialize, Serialize};

use crate::transcription::provider::{TranscriptionCapabilities, TranscriptionProviderId};

pub const GEMINI_LIVE_ENDPOINT: &str = "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent";
pub const DEFAULT_GEMINI_LIVE_MODEL: &str = "gemini-3.1-flash-live-preview";
pub const DEFAULT_GEMINI_AUDIO_CHUNK_MS: u32 = 40;
pub const DEFAULT_MANUAL_ACTIVITY_END_SILENCE_MS: u32 = 600;
pub const DEFAULT_GEMINI_TRANSCRIPT_DRAIN_MS: u32 = 300;
pub const DEFAULT_GEMINI_FINALIZATION_TIMEOUT_MS: u32 = 1_500;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode", content = "tag")]
pub enum LanguageCode {
    Automatic,
    Fixed(String),
}

impl Default for LanguageCode {
    fn default() -> Self {
        Self::Fixed("pt".into())
    }
}

impl From<LanguageCode> for crate::transcription::types::TranscriptionLanguage {
    fn from(value: LanguageCode) -> Self {
        match value {
            LanguageCode::Automatic => Self::Automatic,
            LanguageCode::Fixed(tag) => Self::Fixed(tag),
        }
    }
}

impl From<crate::transcription::types::TranscriptionLanguage> for LanguageCode {
    fn from(value: crate::transcription::types::TranscriptionLanguage) -> Self {
        match value {
            crate::transcription::types::TranscriptionLanguage::Automatic => Self::Automatic,
            crate::transcription::types::TranscriptionLanguage::Fixed(tag) => Self::Fixed(tag),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WhisperLocalSettings {
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeminiLiveSettings {
    #[serde(default = "default_gemini_model")]
    pub model: String,
    #[serde(default = "default_gemini_endpoint")]
    pub endpoint: String,
    #[serde(default = "default_gemini_audio_chunk_ms")]
    pub audio_chunk_ms: u32,
    #[serde(default = "default_manual_activity_end_silence_ms")]
    pub manual_activity_end_silence_ms: u32,
    #[serde(default = "default_gemini_transcript_drain_ms")]
    pub transcript_drain_ms: u32,
    #[serde(default = "default_gemini_finalization_timeout_ms")]
    pub finalization_timeout_ms: u32,
}

impl Default for GeminiLiveSettings {
    fn default() -> Self {
        Self {
            model: default_gemini_model(),
            endpoint: default_gemini_endpoint(),
            audio_chunk_ms: default_gemini_audio_chunk_ms(),
            manual_activity_end_silence_ms: default_manual_activity_end_silence_ms(),
            transcript_drain_ms: default_gemini_transcript_drain_ms(),
            finalization_timeout_ms: default_gemini_finalization_timeout_ms(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct OpenAiRealtimeSettings {
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct OpenAiCompatibleSettings {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub endpoint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TranscriptionProviderSettings {
    #[serde(default)]
    pub whisper_local: WhisperLocalSettings,
    #[serde(default)]
    pub google_gemini: GeminiLiveSettings,
    #[serde(default)]
    pub openai_realtime: OpenAiRealtimeSettings,
    #[serde(default)]
    pub openai_compatible: OpenAiCompatibleSettings,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TranscriptionSettings {
    #[serde(default)]
    pub provider: TranscriptionProviderId,
    #[serde(default)]
    pub language: LanguageCode,
    /// Backward-compatible read path for configuration written before provider-specific
    /// settings existed. New writes leave it empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub providers: TranscriptionProviderSettings,
}

impl TranscriptionSettings {
    pub fn active_model(&self) -> Option<String> {
        let provider_model = match self.provider {
            TranscriptionProviderId::WhisperLocal => self.providers.whisper_local.model.clone(),
            TranscriptionProviderId::GoogleGemini => {
                Some(self.providers.google_gemini.model.clone())
            }
            TranscriptionProviderId::OpenAiRealtime => self.providers.openai_realtime.model.clone(),
            TranscriptionProviderId::OpenAiCompatible => {
                self.providers.openai_compatible.model.clone()
            }
            TranscriptionProviderId::Fake => None,
        };
        self.model.clone().or(provider_model)
    }

    pub fn validate_for(&self, capabilities: TranscriptionCapabilities) -> Result<(), String> {
        if !capabilities.speaker_source_preserved {
            return Err("the transcription provider does not preserve audio source".into());
        }

        match &self.language {
            LanguageCode::Automatic if !capabilities.automatic_language_detection => Err(
                "the transcription provider does not support automatic language detection".into(),
            ),
            LanguageCode::Fixed(tag) if tag.trim().is_empty() => {
                Err("transcription language cannot be empty".into())
            }
            LanguageCode::Fixed(_) if !capabilities.language_selection => {
                Err("the transcription provider does not support language selection".into())
            }
            _ => self.validate_provider_configuration(),
        }
    }

    fn validate_provider_configuration(&self) -> Result<(), String> {
        if self.provider != TranscriptionProviderId::GoogleGemini {
            return Ok(());
        }

        let gemini = &self.providers.google_gemini;
        if gemini.model.trim().is_empty() {
            return Err("Gemini Live model cannot be empty".into());
        }
        if gemini.endpoint != GEMINI_LIVE_ENDPOINT {
            return Err("Gemini Live endpoint must be the official Live API endpoint".into());
        }
        if !matches!(gemini.audio_chunk_ms, 20 | 40) {
            return Err("Gemini Live audio chunk must be 20ms or 40ms".into());
        }
        if !matches!(gemini.manual_activity_end_silence_ms, 500 | 600 | 700 | 800) {
            return Err(
                "Gemini Live manual activity silence must be 500, 600, 700 or 800ms".into(),
            );
        }
        if gemini.transcript_drain_ms == 0
            || gemini.finalization_timeout_ms < gemini.transcript_drain_ms
        {
            return Err("Gemini Live finalization timings are invalid".into());
        }
        Ok(())
    }
}

fn default_gemini_model() -> String {
    DEFAULT_GEMINI_LIVE_MODEL.into()
}

fn default_gemini_endpoint() -> String {
    GEMINI_LIVE_ENDPOINT.into()
}

const fn default_gemini_audio_chunk_ms() -> u32 {
    DEFAULT_GEMINI_AUDIO_CHUNK_MS
}

const fn default_manual_activity_end_silence_ms() -> u32 {
    DEFAULT_MANUAL_ACTIVITY_END_SILENCE_MS
}

const fn default_gemini_transcript_drain_ms() -> u32 {
    DEFAULT_GEMINI_TRANSCRIPT_DRAIN_MS
}

const fn default_gemini_finalization_timeout_ms() -> u32 {
    DEFAULT_GEMINI_FINALIZATION_TIMEOUT_MS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_local_portuguese() {
        let settings = TranscriptionSettings::default();
        assert_eq!(settings.provider, TranscriptionProviderId::WhisperLocal);
        assert_eq!(settings.language, LanguageCode::Fixed("pt".into()));
        assert_eq!(settings.active_model(), None);
    }

    #[test]
    fn language_round_trips_through_provider_type() {
        for code in [LanguageCode::Automatic, LanguageCode::Fixed("en".into())] {
            let provider_language: crate::transcription::types::TranscriptionLanguage =
                code.clone().into();
            assert_eq!(LanguageCode::from(provider_language), code);
        }
    }

    #[test]
    fn capability_validation_rejects_unsupported_language_mode() {
        let settings = TranscriptionSettings::default();
        let mut capabilities = TranscriptionCapabilities::none();
        assert!(settings.validate_for(capabilities).is_err());
        capabilities.language_selection = true;
        assert!(settings.validate_for(capabilities).is_ok());

        let automatic = TranscriptionSettings {
            language: LanguageCode::Automatic,
            ..settings
        };
        assert!(automatic.validate_for(capabilities).is_err());
    }

    #[test]
    fn gemini_configuration_is_typed_and_uses_only_the_official_endpoint() {
        let mut settings = TranscriptionSettings {
            provider: TranscriptionProviderId::GoogleGemini,
            language: LanguageCode::Automatic,
            ..TranscriptionSettings::default()
        };
        let mut capabilities = TranscriptionCapabilities::none();
        capabilities.streaming = true;
        capabilities.partial_results = true;
        capabilities.automatic_language_detection = true;
        capabilities.requires_credentials = true;
        assert!(settings.validate_for(capabilities).is_ok());

        settings.providers.google_gemini.endpoint = "wss://example.invalid".into();
        assert!(settings.validate_for(capabilities).is_err());
    }

    #[test]
    fn gemini_low_latency_tuning_accepts_only_supported_safe_values() {
        let mut settings = TranscriptionSettings {
            provider: TranscriptionProviderId::GoogleGemini,
            language: LanguageCode::Automatic,
            ..TranscriptionSettings::default()
        };
        let mut capabilities = TranscriptionCapabilities::none();
        capabilities.streaming = true;
        capabilities.partial_results = true;
        capabilities.automatic_language_detection = true;
        capabilities.requires_credentials = true;

        for chunk_ms in [20, 40] {
            for silence_ms in [500, 600, 700, 800] {
                settings.providers.google_gemini.audio_chunk_ms = chunk_ms;
                settings
                    .providers
                    .google_gemini
                    .manual_activity_end_silence_ms = silence_ms;
                assert!(settings.validate_for(capabilities).is_ok());
            }
        }
        settings.providers.google_gemini.audio_chunk_ms = 100;
        assert!(settings.validate_for(capabilities).is_err());
        settings.providers.google_gemini.audio_chunk_ms = 40;
        settings
            .providers
            .google_gemini
            .manual_activity_end_silence_ms = 400;
        assert!(settings.validate_for(capabilities).is_err());
    }

    #[test]
    fn legacy_model_remains_readable() {
        let settings: TranscriptionSettings = serde_json::from_str(
            r#"{"provider":"whisper_local","language":{"mode":"fixed","tag":"pt"},"model":"legacy.bin"}"#,
        )
        .unwrap();
        assert_eq!(settings.active_model().as_deref(), Some("legacy.bin"));
    }
}
