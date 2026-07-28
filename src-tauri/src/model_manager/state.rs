//! Estados explícitos do ciclo de vida de um modelo gerenciado — nunca booleanos soltos
//! (mesma convenção usada em `audio::types::AudioCaptureEvent` / `transcription::types::TranscriptEvent`).
//!
//! A especificação original listava "Paused ou Cancelled" como estado após uma
//! interrupção — apenas `Cancelled` foi implementado (não há retomada de download
//! parcial); `Paused` foi deliberadamente omitido para não deixar uma variante morta.

use serde::Serialize;

use crate::model_manager::catalog::ModelLanguageSupport;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ModelInstallState {
    NotInstalled,
    Checking,
    Downloading,
    Cancelled,
    Verifying,
    Installing,
    Ready,
    Corrupted { reason: String },
    Failed { reason: String },
}

/// Snapshot enviado ao frontend: estado atual combinado com os metadados do catálogo
/// necessários para renderizar a tela de onboarding (nome, propósito, tamanho aproximado).
/// `custom_model_path` só é preenchido quando a seleção persistida é um modelo local
/// personalizado (Configurações → Transcrição → Avançado), em vez do modelo padrão.
/// `language_support` é `None` nesse mesmo caso — um modelo personalizado não tem
/// suporte de idioma conhecido pelo catálogo.
#[derive(Debug, Clone, Serialize)]
pub struct ModelStatus {
    pub model_id: &'static str,
    pub display_name: &'static str,
    pub approximate_size_bytes: u64,
    pub state: ModelInstallState,
    pub custom_model_path: Option<String>,
    pub language_support: Option<ModelLanguageSupport>,
}
