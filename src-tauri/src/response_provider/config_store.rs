//! Configuração persistida do provedor de sugestão de resposta (LLM): qual provedor,
//! qual modelo, e um `base_url` opcional (auto-hospedagem/proxy). API keys de provedores
//! de nuvem NUNCA ficam aqui — vão para o keychain do SO via `secrets.rs`. Mesmo padrão
//! de leitura/escrita atômica de `model_manager::config_store`.

use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseProviderKind {
    Ollama,
    OpenAi,
    DeepSeek,
    Anthropic,
}

impl ResponseProviderKind {
    pub fn requires_api_key(self) -> bool {
        !matches!(self, ResponseProviderKind::Ollama)
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
}

impl Default for ResponseProviderConfig {
    fn default() -> Self {
        ResponseProviderConfig {
            provider: ResponseProviderKind::Ollama,
            model: "llama3.1".to_string(),
            base_url: None,
            ollama_keep_alive: default_ollama_keep_alive(),
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
}
