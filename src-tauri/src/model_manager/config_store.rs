//! Seleção de modelo de transcrição persistida em disco. Deliberadamente separada de
//! qualquer configuração futura de `ResponseProvider` (Ollama/OpenAI/Gemini) — a escolha
//! de qual modelo transcreve o áudio nunca é acoplada à escolha de qual provedor gera
//! respostas.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::model_manager::error::ModelManagerError;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TranscriptionModelSelection {
    /// Modelo padrão, obtido e verificado pelo fluxo de download guiado.
    Managed { model_id: String },
    /// Modelo local escolhido manualmente por um usuário avançado (Configurações →
    /// Transcrição → Avançado → Usar modelo local personalizado).
    Custom { model_path: String },
}

pub fn load(path: &Path) -> Result<Option<TranscriptionModelSelection>, ModelManagerError> {
    if !path.is_file() {
        return Ok(None);
    }
    let contents =
        std::fs::read_to_string(path).map_err(|e| ModelManagerError::Disk(e.to_string()))?;
    let selection = serde_json::from_str(&contents)
        .map_err(|e| ModelManagerError::Disk(format!("invalid config file: {e}")))?;
    Ok(Some(selection))
}

/// Grava atomicamente: escreve em um arquivo temporário e renomeia por cima do destino,
/// mesmo padrão usado para o modelo baixado (`.part` → nome final).
pub fn save(path: &Path, selection: &TranscriptionModelSelection) -> Result<(), ModelManagerError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ModelManagerError::Disk(e.to_string()))?;
    }
    let json = serde_json::to_string_pretty(selection)
        .map_err(|e| ModelManagerError::Disk(e.to_string()))?;
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, json).map_err(|e| ModelManagerError::Disk(e.to_string()))?;
    std::fs::rename(&tmp_path, path).map_err(|e| ModelManagerError::Disk(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_config_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "helppye-config-test-{name}-{}.json",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn missing_config_file_loads_as_none() {
        let path = temp_config_path("missing");
        assert_eq!(load(&path).unwrap(), None);
    }

    #[test]
    fn saved_configuration_round_trips_through_load() {
        let path = temp_config_path("roundtrip");
        let selection = TranscriptionModelSelection::Managed {
            model_id: "whisper-base-multilingual".into(),
        };

        save(&path, &selection).unwrap();
        let loaded = load(&path).unwrap();

        assert_eq!(loaded, Some(selection));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn custom_selection_round_trips_through_load() {
        let path = temp_config_path("custom-roundtrip");
        let selection = TranscriptionModelSelection::Custom {
            model_path: "/home/user/models/my-model.bin".into(),
        };

        save(&path, &selection).unwrap();
        let loaded = load(&path).unwrap();

        assert_eq!(loaded, Some(selection));
        std::fs::remove_file(&path).ok();
    }
}
