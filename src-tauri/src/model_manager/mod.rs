//! Fluxo de download guiado do modelo padrão de transcrição no primeiro uso — nunca
//! silencioso (ver `docs/local-transcription.md`). `catalog` centraliza a definição do
//! modelo (id, nome, URL, tamanho, checksum); nenhum desses valores aparece em nenhum
//! outro módulo.

pub mod catalog;
pub mod config_store;
pub mod downloader;
pub mod error;
pub mod events;
pub mod manager;
pub mod paths;
pub mod state;
pub mod verify;

use std::sync::Arc;

use tauri::{AppHandle, Emitter, State};

use downloader::ReqwestDownloader;
use events::MODEL_DOWNLOAD_EVENT;
use manager::ModelManager;
use state::ModelStatus;

use crate::transcription::segment_transcriber::SegmentTranscriber;

pub struct ModelManagerState(pub Arc<ModelManager>);

/// Constrói o `ModelManager` gerenciado — chamado uma vez em `.setup()`. Único ponto
/// fora de `paths::resolve_paths` que toca o `AppHandle` diretamente (aqui, só para
/// encaminhar eventos ao frontend).
pub fn build(
    app: &AppHandle,
    provider: Arc<dyn SegmentTranscriber>,
) -> Result<ModelManagerState, String> {
    let (models_dir, config_path) = paths::resolve_paths(app).map_err(|e| e.to_string())?;
    let app_handle = app.clone();
    let manager = ModelManager::new(
        models_dir,
        config_path,
        Arc::new(ReqwestDownloader::new()),
        provider,
        move |event| {
            if let Err(e) = app_handle.emit(MODEL_DOWNLOAD_EVENT, &event) {
                tracing::warn!(%e, "failed to emit model download event to frontend");
            }
        },
    );
    Ok(ModelManagerState(Arc::new(manager)))
}

#[tauri::command]
pub async fn model_status_command(
    state: State<'_, ModelManagerState>,
) -> Result<ModelStatus, String> {
    state.0.status_snapshot().await.map_err(|e| e.to_string())
}

/// Dispara o download em segundo plano e retorna imediatamente — o progresso e o
/// resultado chegam ao frontend via eventos `MODEL_DOWNLOAD_EVENT`, nunca de forma
/// silenciosa.
#[tauri::command]
pub async fn start_model_download_command(
    state: State<'_, ModelManagerState>,
) -> Result<(), String> {
    let manager = state.0.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = manager.download_default_model().await {
            tracing::warn!(%e, "guided model download finished with an error");
        }
    });
    Ok(())
}

#[tauri::command]
pub async fn cancel_model_download_command(
    state: State<'_, ModelManagerState>,
) -> Result<(), String> {
    state.0.cancel_download().await;
    Ok(())
}

/// Configurações → Transcrição → Avançado → Usar modelo local personalizado. Valida o
/// arquivo (carregando-o de fato) antes de persistir a seleção.
#[tauri::command]
pub async fn select_custom_model_command(
    state: State<'_, ModelManagerState>,
    model_path: String,
    model_name: String,
) -> Result<(), String> {
    state
        .0
        .select_custom_model(model_path.into(), model_name)
        .await
        .map_err(|e| e.to_string())
}
