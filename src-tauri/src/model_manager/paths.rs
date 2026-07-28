//! Resolução do diretório de armazenamento de modelos e do arquivo de configuração
//! persistida — sempre a partir do diretório de dados do app fornecido pelo Tauri
//! (`AppHandle::path().app_data_dir()`), nunca do diretório de trabalho, da pasta do
//! executável, do repositório git ou de um caminho absoluto de desenvolvedor. No Windows
//! isso resolve para o equivalente a `%LOCALAPPDATA%\Helppye\`.

use std::path::{Path, PathBuf};

use tauri::{AppHandle, Manager};

use crate::model_manager::error::ModelManagerError;

pub const MODELS_SUBDIR: &str = "models";
pub const CONFIG_FILENAME: &str = "transcription.json";

/// Função pura, testável sem uma instância real de `AppHandle`: apenas junta "models"
/// ao diretório de dados já resolvido pelo Tauri.
pub fn models_subpath(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join(MODELS_SUBDIR)
}

pub fn config_subpath(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join(CONFIG_FILENAME)
}

pub fn ensure_dir(dir: &Path) -> Result<(), ModelManagerError> {
    std::fs::create_dir_all(dir)
        .map_err(|e| ModelManagerError::DirectoryCreationFailed(e.to_string()))
}

/// Único ponto do código que chama `app_data_dir()` diretamente. Resolve e garante a
/// existência do diretório `models/`, e devolve também o caminho do arquivo de
/// configuração persistida.
pub fn resolve_paths(app: &AppHandle) -> Result<(PathBuf, PathBuf), ModelManagerError> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| ModelManagerError::DataDirUnavailable(e.to_string()))?;
    let models_dir = models_subpath(&data_dir);
    let config_path = config_subpath(&data_dir);
    ensure_dir(&models_dir)?;
    Ok((models_dir, config_path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_subpath_appends_models_dir() {
        let data_dir = Path::new("/some/app/data");
        assert_eq!(models_subpath(data_dir), data_dir.join("models"));
    }

    #[test]
    fn config_subpath_appends_config_filename() {
        let data_dir = Path::new("/some/app/data");
        assert_eq!(
            config_subpath(data_dir),
            data_dir.join("transcription.json")
        );
    }

    #[test]
    fn ensure_dir_creates_missing_nested_directories() {
        let tmp = std::env::temp_dir().join(format!("helppye-test-{}", uuid_like()));
        let nested = tmp.join("a").join("b").join("models");
        assert!(!nested.exists());

        ensure_dir(&nested).expect("directory creation should succeed");

        assert!(nested.is_dir());
        std::fs::remove_dir_all(&tmp).ok();
    }

    fn uuid_like() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }
}
