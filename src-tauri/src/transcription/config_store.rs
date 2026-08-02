//! Persistencia da selecao de provider/idioma/modelo de transcricao. Credenciais nao
//! pertencem a este arquivo; providers remotos devem usa-las pelo keychain.

use std::path::Path;

use super::settings::TranscriptionSettings;

pub fn load(path: &Path) -> TranscriptionSettings {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return TranscriptionSettings::default();
    };
    serde_json::from_str(&contents).unwrap_or_else(|error| {
        tracing::warn!(%error, path = %path.display(), "invalid transcription settings; using defaults");
        TranscriptionSettings::default()
    })
}

pub fn save(path: &Path, settings: &TranscriptionSettings) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let encoded = serde_json::to_vec_pretty(settings).map_err(|error| error.to_string())?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, encoded).map_err(|error| error.to_string())?;
    std::fs::rename(&temporary, path).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcription::provider::TranscriptionProviderId;
    use crate::transcription::settings::LanguageCode;

    fn path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "helppye-transcription-settings-{name}-{}.json",
            std::process::id()
        ))
    }

    #[test]
    fn missing_or_corrupt_files_use_safe_defaults() {
        let missing = path("missing");
        let _ = std::fs::remove_file(&missing);
        assert_eq!(load(&missing), TranscriptionSettings::default());

        let corrupt = path("corrupt");
        std::fs::write(&corrupt, "{").unwrap();
        assert_eq!(load(&corrupt), TranscriptionSettings::default());
        let _ = std::fs::remove_file(corrupt);
    }

    #[test]
    fn settings_round_trip_atomically() {
        let target = path("round-trip");
        let settings = TranscriptionSettings {
            provider: TranscriptionProviderId::OpenAiRealtime,
            language: LanguageCode::Automatic,
            model: Some("realtime-model".into()),
            providers: Default::default(),
        };
        save(&target, &settings).unwrap();
        assert_eq!(load(&target), settings);
        assert!(!target.with_extension("json.tmp").exists());
        let _ = std::fs::remove_file(target);
    }
}
