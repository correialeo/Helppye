//! Configuração da camada de transcrição, **separada** da configuração de geração de
//! resposta (`response_provider::settings::ResponseSettings`).
//!
//! São dois campos independentes de propósito: transcrever localmente e gerar na nuvem é
//! uma combinação legítima e é o default do produto. Um único campo "provedor de IA" ligaria
//! as duas escolhas e tornaria impossível expressá-la.

use serde::{Deserialize, Serialize};

use crate::transcription::provider::TranscriptionProviderId;

/// Código de idioma no formato que os backends aceitam (`"pt"`, `"en"`, ...), ou
/// `Automatic` para detecção pelo próprio provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode", content = "tag")]
pub enum LanguageCode {
    Automatic,
    Fixed(String),
}

impl Default for LanguageCode {
    fn default() -> Self {
        LanguageCode::Fixed("pt".into())
    }
}

impl From<LanguageCode> for crate::transcription::types::TranscriptionLanguage {
    fn from(value: LanguageCode) -> Self {
        match value {
            LanguageCode::Automatic => {
                crate::transcription::types::TranscriptionLanguage::Automatic
            }
            LanguageCode::Fixed(tag) => {
                crate::transcription::types::TranscriptionLanguage::Fixed(tag)
            }
        }
    }
}

impl From<crate::transcription::types::TranscriptionLanguage> for LanguageCode {
    fn from(value: crate::transcription::types::TranscriptionLanguage) -> Self {
        match value {
            crate::transcription::types::TranscriptionLanguage::Automatic => {
                LanguageCode::Automatic
            }
            crate::transcription::types::TranscriptionLanguage::Fixed(tag) => {
                LanguageCode::Fixed(tag)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TranscriptionSettings {
    #[serde(default)]
    pub provider: TranscriptionProviderId,
    #[serde(default)]
    pub language: LanguageCode,
    /// Nome/caminho do modelo, quando o provider aceita escolha. `None` = o provider decide
    /// (para o Whisper local, o modelo já carregado).
    #[serde(default)]
    pub model: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_local_portuguese() {
        let settings = TranscriptionSettings::default();
        assert_eq!(settings.provider, TranscriptionProviderId::WhisperLocal);
        assert_eq!(settings.language, LanguageCode::Fixed("pt".into()));
        assert_eq!(settings.model, None);
    }

    #[test]
    fn language_round_trips_through_the_provider_type() {
        for code in [LanguageCode::Automatic, LanguageCode::Fixed("en".into())] {
            let provider_language: crate::transcription::types::TranscriptionLanguage =
                code.clone().into();
            assert_eq!(LanguageCode::from(provider_language), code);
        }
    }
}
