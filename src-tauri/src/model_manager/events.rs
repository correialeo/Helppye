//! Eventos emitidos ao frontend durante o download guiado do modelo, mirando
//! `transcription::TRANSCRIPTION_EVENT` / `audio::CAPTURE_EVENT` na convenção de nomes.

use serde::Serialize;

pub const MODEL_DOWNLOAD_EVENT: &str = "model-download://event";

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelDownloadEvent {
    Started {
        model_id: String,
        total_bytes: u64,
    },
    Progress {
        model_id: String,
        downloaded_bytes: u64,
        total_bytes: u64,
        bytes_per_second: f64,
    },
    Verifying {
        model_id: String,
    },
    Completed {
        model_id: String,
        path: String,
    },
    Cancelled {
        model_id: String,
    },
    Failed {
        model_id: String,
        error: String,
    },
}
