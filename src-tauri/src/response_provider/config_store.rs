//! Configuração persistida do provedor de sugestão de resposta (LLM): qual provedor,
//! qual modelo, e um `base_url` opcional (auto-hospedagem/proxy). API keys de provedores
//! de nuvem NUNCA ficam aqui — vão para o keychain do SO via `secrets.rs`. Mesmo padrão
//! de leitura/escrita atômica de `model_manager::config_store`.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::openai_compatible::CredentialMode;
use super::provider::ResponseProviderId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseProviderKind {
    Ollama,
    /// Servidor local do LM Studio, dialeto compatível com a API da OpenAI.
    LmStudio,
    OpenAi,
    DeepSeek,
    Anthropic,
    OpenRouter,
    /// Endpoint compatível com a API da OpenAI definido inteiramente pelo usuário.
    CustomOpenAiCompatible,
}

impl ResponseProviderKind {
    /// Se o provedor **não funciona** sem credencial no keychain. LM Studio e endpoint
    /// personalizado ficam de fora de propósito: os dois aceitam credencial opcional
    /// (`CredentialMode`), e exigi-la impediria o caso mais comum — um servidor local sem
    /// autenticação nenhuma.
    pub fn requires_api_key(self) -> bool {
        matches!(
            self,
            ResponseProviderKind::OpenAi
                | ResponseProviderKind::DeepSeek
                | ResponseProviderKind::Anthropic
                | ResponseProviderKind::OpenRouter
        )
    }

    /// Se aceita credencial, obrigatória ou não. É o que decide se a UI mostra o campo de
    /// chave de API.
    pub fn accepts_api_key(self) -> bool {
        !matches!(self, ResponseProviderKind::Ollama)
    }

    pub fn id(self) -> ResponseProviderId {
        match self {
            ResponseProviderKind::Ollama => ResponseProviderId::Ollama,
            ResponseProviderKind::LmStudio => ResponseProviderId::LmStudio,
            ResponseProviderKind::OpenAi => ResponseProviderId::OpenAi,
            ResponseProviderKind::DeepSeek => ResponseProviderId::DeepSeek,
            ResponseProviderKind::Anthropic => ResponseProviderId::Anthropic,
            ResponseProviderKind::OpenRouter => ResponseProviderId::OpenRouter,
            ResponseProviderKind::CustomOpenAiCompatible => {
                ResponseProviderId::CustomOpenAiCompatible
            }
        }
    }
}

fn default_ollama_keep_alive() -> Option<String> {
    Some(crate::response_provider::ollama::DEFAULT_KEEP_ALIVE.to_string())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponseProviderConfig {
    pub provider: ResponseProviderKind,
    pub model: String,
    /// Override de endpoint (host do Ollama, proxy compatível com a API da OpenAI, etc.).
    /// `None` usa o padrão de cada provedor.
    pub base_url: Option<String>,
    /// Só usado pelo provider Ollama (`ResponseProviderKind::Ollama`) — quanto tempo o
    /// Ollama mantém o modelo carregado depois de uma chamada, para evitar pagar o custo
    /// de recarregá-lo na chamada seguinte (ver `ollama::DEFAULT_KEEP_ALIVE`). `None`
    /// deixa a critério do padrão do próprio Ollama. `#[serde(default = ...)]` para que
    /// arquivos de configuração salvos antes deste campo existir continuem carregando.
    #[serde(default = "default_ollama_keep_alive")]
    pub ollama_keep_alive: Option<String>,
    /// Como a credencial viaja para provedores compatíveis com a API da OpenAI. Só é
    /// consultado por eles; Ollama e Anthropic têm forma própria e fixa.
    #[serde(default)]
    pub credential_mode: CredentialMode,
    /// Cabeçalhos extras exigidos por alguns gateways (`X-Title` no OpenRouter, cabeçalho
    /// de roteamento num proxy corporativo). Pares nome/valor, validados por
    /// `endpoint::build_custom_headers` — reservados são recusados, e os valores nunca
    /// entram em log. **Não** são persistidos como local de segredo: uma credencial
    /// pertence ao keychain, e este campo vai para um JSON em texto puro no disco.
    #[serde(default)]
    pub custom_headers: Vec<(String, String)>,
}

impl Default for ResponseProviderConfig {
    fn default() -> Self {
        ResponseProviderConfig {
            provider: ResponseProviderKind::Ollama,
            model: "llama3.1".to_string(),
            base_url: None,
            ollama_keep_alive: default_ollama_keep_alive(),
            credential_mode: CredentialMode::default(),
            custom_headers: Vec::new(),
        }
    }
}

/// Carrega a configuração salva; qualquer ausência ou corrupção do arquivo cai para o
/// padrão (Ollama local) em vez de impedir a inicialização do app.
pub fn load(path: &Path) -> ResponseProviderConfig {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return ResponseProviderConfig::default();
    };
    match serde_json::from_str(&contents) {
        Ok(config) => config,
        Err(e) => {
            tracing::warn!(
                %e,
                "invalid response provider config file, falling back to default"
            );
            ResponseProviderConfig::default()
        }
    }
}

pub fn save(path: &Path, config: &ResponseProviderConfig) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, json).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp_path, path).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_config_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "helppye-response-provider-config-test-{name}-{}.json",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn missing_config_file_loads_as_default() {
        let path = temp_config_path("missing");
        assert_eq!(load(&path), ResponseProviderConfig::default());
    }

    #[test]
    fn corrupted_config_file_loads_as_default() {
        let path = temp_config_path("corrupted");
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(load(&path), ResponseProviderConfig::default());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn saved_configuration_round_trips_through_load() {
        let path = temp_config_path("roundtrip");
        let config = ResponseProviderConfig {
            provider: ResponseProviderKind::Anthropic,
            model: "claude-sonnet".to_string(),
            base_url: Some("https://example.com".to_string()),
            ollama_keep_alive: Some("5m".to_string()),
            credential_mode: CredentialMode::BearerToken,
            custom_headers: vec![("x-title".to_string(), "Helppye".to_string())],
        };

        save(&path, &config).unwrap();
        assert_eq!(load(&path), config);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn config_missing_keep_alive_field_falls_back_to_default() {
        let path = temp_config_path("missing-keep-alive-field");
        std::fs::write(
            &path,
            r#"{"provider":"ollama","model":"llama3.1","base_url":null}"#,
        )
        .unwrap();

        let config = load(&path);
        assert_eq!(config.ollama_keep_alive, default_ollama_keep_alive());
        std::fs::remove_file(&path).ok();
    }

    /// Os campos de endpoint personalizado nasceram depois; um arquivo salvo antes deles
    /// tem que continuar carregando com os padrões, e não derrubar o usuário para uma
    /// configuração inteiramente padrão (o que trocaria o provedor dele por Ollama).
    #[test]
    fn config_saved_before_custom_endpoint_fields_still_loads() {
        let path = temp_config_path("pre-custom-endpoint");
        std::fs::write(
            &path,
            r#"{"provider":"deep_seek","model":"deepseek-chat","base_url":null,"ollama_keep_alive":"10m"}"#,
        )
        .unwrap();

        let config = load(&path);
        assert_eq!(config.provider, ResponseProviderKind::DeepSeek);
        assert_eq!(config.model, "deepseek-chat");
        assert_eq!(config.credential_mode, CredentialMode::BearerToken);
        assert!(config.custom_headers.is_empty());
        std::fs::remove_file(&path).ok();
    }

    /// LM Studio e endpoint personalizado aceitam credencial, mas não a exigem: o caso
    /// normal é um servidor local sem autenticação nenhuma.
    #[test]
    fn only_cloud_providers_require_an_api_key() {
        for kind in [
            ResponseProviderKind::OpenAi,
            ResponseProviderKind::DeepSeek,
            ResponseProviderKind::Anthropic,
            ResponseProviderKind::OpenRouter,
        ] {
            assert!(kind.requires_api_key(), "{kind:?}");
            assert!(kind.accepts_api_key(), "{kind:?}");
        }
        for kind in [
            ResponseProviderKind::LmStudio,
            ResponseProviderKind::CustomOpenAiCompatible,
        ] {
            assert!(!kind.requires_api_key(), "{kind:?}");
            assert!(kind.accepts_api_key(), "{kind:?}");
        }
        assert!(!ResponseProviderKind::Ollama.requires_api_key());
        assert!(!ResponseProviderKind::Ollama.accepts_api_key());
    }
}
