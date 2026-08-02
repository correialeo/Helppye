//! Transcription credentials stored exclusively in the operating-system keychain.

use thiserror::Error;

use crate::transcription::provider::TranscriptionProviderId;

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

fn account_for(provider: TranscriptionProviderId) -> Option<&'static str> {
    match provider {
        TranscriptionProviderId::GoogleGemini => Some("gemini-live-transcription-api-key"),
        _ => None,
    }
}

fn entry_for(provider: TranscriptionProviderId) -> Result<keyring::Entry, SecretError> {
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

pub fn store_api_key(provider: TranscriptionProviderId, api_key: &str) -> Result<(), SecretError> {
    if api_key.trim().is_empty() {
        return Err(SecretError::NotFound);
    }
    entry_for(provider)?
        .set_password(api_key.trim())
        .map_err(map_keyring_error)
}

pub fn load_api_key(provider: TranscriptionProviderId) -> Result<Option<String>, SecretError> {
    match entry_for(provider)?.get_password() {
        Ok(password) => Ok(Some(password)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(map_keyring_error(error)),
    }
}

pub fn delete_api_key(provider: TranscriptionProviderId) -> Result<(), SecretError> {
    match entry_for(provider)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(map_keyring_error(error)),
    }
}

pub fn has_api_key(provider: TranscriptionProviderId) -> Result<bool, SecretError> {
    if account_for(provider).is_none() {
        return Ok(false);
    }
    load_api_key(provider).map(|key| key.is_some_and(|value| !value.trim().is_empty()))
}
