//! Configuração da camada de geração de resposta, **separada** da configuração de
//! transcrição (`transcription::settings::TranscriptionSettings`).
//!
//! O par é o ponto: `TranscriptionSettings::provider` e `ResponseSettings::provider` são
//! dois campos, não um. Transcrever localmente com Whisper e gerar com um modelo de nuvem é
//! a combinação padrão do produto; um único seletor de "provedor de IA" tornaria essa
//! combinação inexprimível — e ligaria a escolha mais sensível (para onde vai o áudio) à
//! menos sensível (qual LLM escreve a sugestão).
//!
//! `ResponseProviderConfig` (`config_store.rs`) continua sendo o formato **persistido**,
//! com os campos de transporte (`base_url`, `credential_mode`, `custom_headers`,
//! `ollama_keep_alive`). `ResponseSettings` é a vista mínima — provedor e modelo — que
//! espelha `TranscriptionSettings` e é o que a UI de configuração combina livremente.

use serde::{Deserialize, Serialize};

use super::config_store::{ResponseProviderConfig, ResponseProviderKind};
use super::provider::ResponseProviderId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseSettings {
    #[serde(default = "default_provider")]
    pub provider: ResponseProviderId,
    #[serde(default = "default_model")]
    pub model: String,
}

fn default_provider() -> ResponseProviderId {
    ResponseProviderId::Ollama
}

fn default_model() -> String {
    ResponseProviderConfig::default().model
}

impl Default for ResponseSettings {
    fn default() -> Self {
        ResponseSettings {
            provider: default_provider(),
            model: default_model(),
        }
    }
}

impl From<&ResponseProviderConfig> for ResponseSettings {
    fn from(config: &ResponseProviderConfig) -> Self {
        ResponseSettings {
            provider: config.provider.id(),
            model: config.model.clone(),
        }
    }
}

impl ResponseSettings {
    /// Converte de volta para o tipo persistido, preservando o transporte já configurado.
    /// `None` significa "nenhum id de provedor conhecido corresponde" — hoje só o caso do
    /// `ChatGptCodexAccount`, que não é construível (ver
    /// `docs/adr/chatgpt-codex-subscription-auth.md`), e do `Misconfigured`, que é estado
    /// de erro e não escolha.
    pub fn apply_to(&self, base: &ResponseProviderConfig) -> Option<ResponseProviderConfig> {
        let provider = kind_for(self.provider)?;
        Some(ResponseProviderConfig {
            provider,
            model: self.model.clone(),
            ..base.clone()
        })
    }
}

fn kind_for(id: ResponseProviderId) -> Option<ResponseProviderKind> {
    match id {
        ResponseProviderId::Ollama => Some(ResponseProviderKind::Ollama),
        ResponseProviderId::LmStudio => Some(ResponseProviderKind::LmStudio),
        ResponseProviderId::OpenAi => Some(ResponseProviderKind::OpenAi),
        ResponseProviderId::DeepSeek => Some(ResponseProviderKind::DeepSeek),
        ResponseProviderId::Anthropic => Some(ResponseProviderKind::Anthropic),
        ResponseProviderId::OpenRouter => Some(ResponseProviderKind::OpenRouter),
        ResponseProviderId::CustomOpenAiCompatible => {
            Some(ResponseProviderKind::CustomOpenAiCompatible)
        }
        ResponseProviderId::ChatGptCodexAccount | ResponseProviderId::Misconfigured => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcription::provider::TranscriptionProviderId;
    use crate::transcription::settings::TranscriptionSettings;

    #[test]
    fn default_is_local_ollama() {
        let settings = ResponseSettings::default();
        assert_eq!(settings.provider, ResponseProviderId::Ollama);
        assert_eq!(settings.model, ResponseProviderConfig::default().model);
    }

    /// O requisito literal: provedor de transcrição e provedor de resposta não podem ser o
    /// mesmo campo. Este teste prova que são tipos distintos e combináveis — Whisper local
    /// com geração na nuvem é uma configuração expressável.
    #[test]
    fn transcription_and_response_providers_are_independent_fields() {
        let transcription = TranscriptionSettings {
            provider: TranscriptionProviderId::WhisperLocal,
            ..Default::default()
        };
        let response = ResponseSettings {
            provider: ResponseProviderId::OpenAi,
            model: "gpt-4o-mini".to_string(),
        };

        assert_eq!(
            transcription.provider,
            TranscriptionProviderId::WhisperLocal
        );
        assert_eq!(response.provider, ResponseProviderId::OpenAi);
    }

    #[test]
    fn round_trips_through_the_persisted_config() {
        let base = ResponseProviderConfig {
            provider: ResponseProviderKind::LmStudio,
            model: "qwen2.5".to_string(),
            base_url: Some("http://localhost:1234/v1".to_string()),
            ..Default::default()
        };
        let settings = ResponseSettings::from(&base);
        assert_eq!(settings.provider, ResponseProviderId::LmStudio);

        let applied = settings.apply_to(&base).unwrap();
        assert_eq!(applied, base);
    }

    /// Trocar de provedor preserva o transporte já configurado — trocar o modelo não pode
    /// apagar `base_url` nem os cabeçalhos que o usuário digitou.
    #[test]
    fn applying_settings_preserves_transport_configuration() {
        let base = ResponseProviderConfig {
            provider: ResponseProviderKind::CustomOpenAiCompatible,
            model: "antigo".to_string(),
            base_url: Some("https://proxy.example.com/v1".to_string()),
            custom_headers: vec![("x-title".to_string(), "Helppye".to_string())],
            ..Default::default()
        };
        let applied = ResponseSettings {
            provider: ResponseProviderId::CustomOpenAiCompatible,
            model: "novo".to_string(),
        }
        .apply_to(&base)
        .unwrap();

        assert_eq!(applied.model, "novo");
        assert_eq!(applied.base_url, base.base_url);
        assert_eq!(applied.custom_headers, base.custom_headers);
    }

    /// Um provedor não construível não vira configuração persistida: seria salvar uma
    /// escolha que nenhuma geração conseguiria honrar.
    #[test]
    fn unconstructible_providers_do_not_produce_a_config() {
        let base = ResponseProviderConfig::default();
        for id in [
            ResponseProviderId::ChatGptCodexAccount,
            ResponseProviderId::Misconfigured,
        ] {
            let settings = ResponseSettings {
                provider: id,
                model: "x".to_string(),
            };
            assert!(settings.apply_to(&base).is_none(), "{id:?}");
        }
    }
}
