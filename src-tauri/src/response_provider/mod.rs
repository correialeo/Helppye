//! Sugestão de resposta em streaming a partir de turnos elegíveis da Conversation
//! Timeline. Provedor de LLM escolhido pelo usuário (Ollama local por padrão, ou OpenAI/
//! DeepSeek/Anthropic na nuvem, com API key no keychain do SO — nunca em texto puro). Ver
//! `docs/response-suggestion.md`.

pub mod anthropic;
pub mod config_store;
pub mod context;
pub mod context_leak_guard;
pub mod echo_guard;
pub mod endpoint;
pub mod engine;
pub mod events;
pub mod net;
pub mod ollama;
pub mod openai_compatible;
pub mod provider;
pub mod registry;
pub mod secrets;
pub mod settings;
pub mod skip_detector;
pub mod validation;

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
    pub accepts_api_key: bool,
    pub has_api_key: bool,
    /// Como o endpoint efetivamente configurado é classificado, e sua forma sanitizada.
    /// `None` quando o provedor usa o padrão dele. É o que permite a UI dizer "isso sai da
    /// sua máquina" antes de a primeira sugestão ser gerada, em vez de depois.
    pub endpoint: Option<EndpointStatus>,
    /// Capacidades da instância viva, não as do catálogo — ver
    /// `ResponseEngine::active_capabilities`.
    pub capabilities: provider::ResponseProviderCapabilities,
}

#[derive(Debug, Serialize)]
pub struct EndpointStatus {
    /// Apenas `esquema://host:porta` — nunca caminho, query ou credencial.
    pub sanitized: String,
    pub classification: endpoint::EndpointClassification,
    pub leaves_machine: bool,
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
    let accepts_api_key = config.provider.accepts_api_key();
    let has_api_key = if accepts_api_key {
        secrets::has_api_key(config.provider).map_err(secret_error_message)?
    } else {
        false
    };
    let endpoint = config
        .base_url
        .as_deref()
        .and_then(|raw| endpoint::validate_base_url(raw).ok())
        .map(|validated| EndpointStatus {
            sanitized: validated.sanitized(),
            classification: validated.classification(),
            leaves_machine: validated.classification().leaves_machine(),
        });
    Ok(ResponseProviderStatus {
        config,
        requires_api_key,
        accepts_api_key,
        has_api_key,
        endpoint,
        capabilities: state.0.active_capabilities(),
    })
}

/// Catálogo de provedores de resposta, com capacidades declaradas e o motivo real de
/// indisponibilidade dos que não existem nesta build.
#[tauri::command]
pub async fn response_providers_command() -> Vec<registry::ResponseProviderDescriptor> {
    registry::descriptors()
}

#[tauri::command]
// Tauri maps these flat arguments directly from the existing frontend payload. Wrapping
// them would be a breaking command-shape change for one optional defensive setting.
#[allow(clippy::too_many_arguments)]
pub async fn response_set_provider_config_command(
    state: State<'_, ResponseEngineState>,
    provider: ResponseProviderKind,
    model: String,
    base_url: Option<String>,
    ollama_keep_alive: Option<String>,
    maximum_automatic_generation_age_ms: Option<u64>,
    credential_mode: Option<openai_compatible::CredentialMode>,
    custom_headers: Option<Vec<(String, String)>>,
) -> Result<(), String> {
    let custom_headers = custom_headers.unwrap_or_default();
    // Validar **antes** de persistir. Salvar um endpoint inválido e só descobrir na
    // primeira geração significa descobrir no meio de uma reunião.
    if let Some(raw) = base_url.as_deref() {
        let validated = endpoint::validate_base_url(raw).map_err(|e| e.to_string())?;
        if validated.classification().leaves_machine() {
            tracing::info!(
                provider = provider.id().as_str(),
                endpoint = %validated.sanitized(),
                classification = ?validated.classification(),
                "endpoint remoto configurado: o conteúdo da reunião será enviado para fora da máquina"
            );
        }
    }
    endpoint::build_custom_headers(&custom_headers).map_err(|e| e.to_string())?;

    state.0.update_config(ResponseProviderConfig {
        provider,
        model,
        base_url,
        ollama_keep_alive,
        maximum_automatic_generation_age_ms: maximum_automatic_generation_age_ms
            .unwrap_or_else(|| state.0.current_config().maximum_automatic_generation_age_ms)
            .clamp(1_000, 300_000),
        credential_mode: credential_mode.unwrap_or_default(),
        custom_headers,
    })
}

/// Vista mínima (provedor + modelo) da configuração de geração, espelhando
/// `transcription_settings_command`. Existe para que a UI possa tratar as duas escolhas —
/// transcrever e gerar — como dois campos independentes.
#[tauri::command]
pub async fn response_settings_command(
    state: State<'_, ResponseEngineState>,
) -> Result<settings::ResponseSettings, String> {
    Ok(settings::ResponseSettings::from(&state.0.current_config()))
}

#[tauri::command]
pub async fn response_set_settings_command(
    state: State<'_, ResponseEngineState>,
    settings: settings::ResponseSettings,
) -> Result<(), String> {
    if !registry::is_available(settings.provider) {
        let reason = registry::unavailable_reason(settings.provider)
            .unwrap_or("provedor de resposta desconhecido");
        return Err(format!("{}: {reason}", settings.provider.as_str()));
    }
    let base = state.0.current_config();
    let config = settings
        .apply_to(&base)
        .ok_or_else(|| format!("{} não é selecionável", settings.provider.as_str()))?;
    state.0.update_config(config)
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
