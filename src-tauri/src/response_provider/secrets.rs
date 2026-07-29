//! Armazenamento de API keys de provedores de nuvem no keychain do SO (Windows
//! Credential Manager, macOS Keychain, Secret Service no Linux) via crate `keyring`.
//! Nunca persistidas em texto puro — a configuração não-secreta (provedor/modelo/
//! base_url) fica em `config_store.rs`. Ambientes sem um backend de keychain utilizável
//! (ex.: este sandbox Linux/WSL2 sem Secret Service em execução) devolvem um erro
//! explícito em vez de falhar silenciosamente ou gravar em texto puro.

use thiserror::Error;

use super::config_store::ResponseProviderKind;

const KEYRING_SERVICE: &str = "helppye";

#[derive(Debug, Error)]
pub enum SecretError {
    #[error("keychain do sistema indisponível: {0}")]
    Unavailable(String),
    #[error("nenhuma chave salva para este provedor")]
    NotFound,
    #[error("este provedor não usa API key")]
    NotApplicable,
    #[error("falha ao acessar o keychain: {0}")]
    Backend(String),
}

fn account_for(provider: ResponseProviderKind) -> Option<&'static str> {
    match provider {
        ResponseProviderKind::Ollama => None,
        ResponseProviderKind::OpenAi => Some("openai-api-key"),
        ResponseProviderKind::DeepSeek => Some("deepseek-api-key"),
        ResponseProviderKind::Anthropic => Some("anthropic-api-key"),
    }
}

fn entry_for(provider: ResponseProviderKind) -> Result<keyring::Entry, SecretError> {
    let account = account_for(provider).ok_or(SecretError::NotApplicable)?;
    keyring::Entry::new(KEYRING_SERVICE, account).map_err(map_keyring_error)
}

fn map_keyring_error(error: keyring::Error) -> SecretError {
    match error {
        keyring::Error::NoEntry => SecretError::NotFound,
        keyring::Error::NoStorageAccess(_) | keyring::Error::NoDefaultStore => {
            SecretError::Unavailable(error.to_string())
        }
        other => SecretError::Backend(other.to_string()),
    }
}

pub fn store_api_key(provider: ResponseProviderKind, api_key: &str) -> Result<(), SecretError> {
    entry_for(provider)?
        .set_password(api_key)
        .map_err(map_keyring_error)
}

pub fn load_api_key(provider: ResponseProviderKind) -> Result<Option<String>, SecretError> {
    match entry_for(provider)?.get_password() {
        Ok(password) => Ok(Some(password)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(map_keyring_error(e)),
    }
}

pub fn delete_api_key(provider: ResponseProviderKind) -> Result<(), SecretError> {
    match entry_for(provider)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(map_keyring_error(e)),
    }
}

pub fn has_api_key(provider: ResponseProviderKind) -> Result<bool, SecretError> {
    match provider {
        ResponseProviderKind::Ollama => Ok(false),
        _ => Ok(load_api_key(provider)?.is_some()),
    }
}
