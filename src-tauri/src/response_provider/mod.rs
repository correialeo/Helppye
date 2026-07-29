//! Sugestão de resposta em streaming a partir de turnos elegíveis da Conversation
//! Timeline. Provedor de LLM escolhido pelo usuário (Ollama local por padrão, ou OpenAI/
//! DeepSeek/Anthropic na nuvem, com API key no keychain do SO — nunca em texto puro). Ver
//! `docs/response-suggestion.md`.

pub mod anthropic;
pub mod config_store;
pub mod context;
pub mod engine;
pub mod events;
pub mod net;
pub mod ollama;
pub mod openai_compatible;
pub mod provider;
pub mod secrets;
pub mod skip_detector;

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use config_store::{ResponseProviderConfig, ResponseProviderKind};
use engine::ResponseEngine;
use secrets::SecretError;

pub struct ResponseEngineState(pub Arc<ResponseEngine>);

const CONFIG_FILENAME: &str = "response_provider.json";

/// Constrói o `ResponseEngine` gerenciado — chamado uma vez em `.setup()`. Nunca falha:
/// se o diretório de dados do app não puder ser resolvido, cai para um caminho relativo
/// em vez de impedir a inicialização (o pior caso é a configuração não persistir entre
/// execuções, não um app que não abre).
pub fn build(app: &AppHandle) -> ResponseEngineState {
    let config_path = resolve_config_path(app);
    ResponseEngineState(Arc::new(ResponseEngine::from_config_path(config_path)))
}

fn resolve_config_path(app: &AppHandle) -> PathBuf {
    match app.path().app_data_dir() {
        Ok(dir) => dir.join(CONFIG_FILENAME),
        Err(e) => {
            tracing::warn!(
                %e,
                "app data dir unavailable, response provider config will not persist reliably"
            );
            PathBuf::from(CONFIG_FILENAME)
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ResponseProviderStatus {
    #[serde(flatten)]
    pub config: ResponseProviderConfig,
    pub requires_api_key: bool,
    pub has_api_key: bool,
}

fn secret_error_message(e: SecretError) -> String {
    e.to_string()
}

#[tauri::command]
pub async fn response_provider_status_command(
    state: State<'_, ResponseEngineState>,
) -> Result<ResponseProviderStatus, String> {
    let config = state.0.current_config();
    let requires_api_key = config.provider.requires_api_key();
    let has_api_key = if requires_api_key {
        secrets::has_api_key(config.provider).map_err(secret_error_message)?
    } else {
        false
    };
    Ok(ResponseProviderStatus {
        config,
        requires_api_key,
        has_api_key,
    })
}

#[tauri::command]
pub async fn response_set_provider_config_command(
    state: State<'_, ResponseEngineState>,
    provider: ResponseProviderKind,
    model: String,
    base_url: Option<String>,
) -> Result<(), String> {
    state.0.update_config(ResponseProviderConfig {
        provider,
        model,
        base_url,
    })
}

#[tauri::command]
pub async fn response_set_api_key_command(
    state: State<'_, ResponseEngineState>,
    provider: ResponseProviderKind,
    api_key: String,
) -> Result<(), String> {
    secrets::store_api_key(provider, &api_key).map_err(secret_error_message)?;
    state.0.reload_provider_if_current(provider);
    Ok(())
}

#[tauri::command]
pub async fn response_delete_api_key_command(
    state: State<'_, ResponseEngineState>,
    provider: ResponseProviderKind,
) -> Result<(), String> {
    secrets::delete_api_key(provider).map_err(secret_error_message)?;
    state.0.reload_provider_if_current(provider);
    Ok(())
}
