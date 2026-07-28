//! Seleção determinística de dispositivo por categoria (entrada/saída): resolução pura
//! (testável sem hardware, ver `resolve_device`) e persistência do ID selecionado.
//!
//! Nunca persiste nome de dispositivo, apenas `id` (estável) — ver convenção documentada
//! em `CLAUDE.md` sobre não usar substring de nome para identificar dispositivos.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::audio::types::AudioDevice;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceSelectionConfig {
    pub input_device_id: Option<String>,
    pub output_device_id: Option<String>,
}

/// De onde veio um `ResolvedDevice`: informa ao frontend qual texto explicativo mostrar
/// ("Seleção manual" vs "Padrão do Windows" vs o aviso de fallback para o primeiro
/// dispositivo disponível).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionSource {
    /// O ID persistido (restaurado de uma seleção anterior, manual ou não) ainda existe.
    Persisted,
    /// Nenhuma seleção persistida (ou ela não existe mais) e o dispositivo indicado pelo
    /// SO como padrão foi usado.
    WindowsDefault,
    /// Nenhuma seleção persistida e nenhum dispositivo padrão do SO foi encontrado —
    /// primeiro dispositivo da lista usado como último recurso.
    FirstAvailableFallback,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ResolvedDevice {
    pub device_id: String,
    pub device_name: String,
    pub is_windows_default: bool,
    pub source: ResolutionSource,
}

/// Resolve qual dispositivo deve estar selecionado para uma categoria (entrada OU saída),
/// dada a lista de dispositivos atualmente disponíveis e o ID persistido (se houver).
///
/// Ordem de prioridade, na ordem exata especificada: (1) ID persistido, se ainda presente
/// na lista — mesmo que um *outro* dispositivo tenha se tornado o padrão do SO nesse
/// meio-tempo, a seleção persistida nunca é substituída silenciosamente; (2) o dispositivo
/// marcado como padrão do SO; (3) o primeiro dispositivo da lista, como último recurso.
/// Retorna `None` apenas quando `devices` está vazia.
///
/// Função pura, sem I/O — testável sem qualquer hardware de áudio real.
pub fn resolve_device(
    devices: &[AudioDevice],
    persisted_id: Option<&str>,
) -> Option<ResolvedDevice> {
    if let Some(pid) = persisted_id {
        if let Some(d) = devices.iter().find(|d| d.id == pid) {
            return Some(ResolvedDevice {
                device_id: d.id.clone(),
                device_name: d.name.clone(),
                is_windows_default: d.is_default,
                source: ResolutionSource::Persisted,
            });
        }
    }

    if let Some(d) = devices.iter().find(|d| d.is_default) {
        return Some(ResolvedDevice {
            device_id: d.id.clone(),
            device_name: d.name.clone(),
            is_windows_default: true,
            source: ResolutionSource::WindowsDefault,
        });
    }

    devices.first().map(|d| ResolvedDevice {
        device_id: d.id.clone(),
        device_name: d.name.clone(),
        is_windows_default: false,
        source: ResolutionSource::FirstAvailableFallback,
    })
}

/// Carrega a seleção persistida. Tolerante: arquivo ausente ou corrompido resulta em
/// `DeviceSelectionConfig::default()` (nenhuma seleção), nunca um erro fatal — o
/// fallback de `resolve_device` cobre esse caso normalmente.
pub fn load(path: &Path) -> DeviceSelectionConfig {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Grava atomicamente: escreve em um arquivo temporário e renomeia por cima do destino,
/// mesmo padrão usado em `model_manager::config_store`.
pub fn save(path: &Path, config: &DeviceSelectionConfig) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json =
        serde_json::to_string_pretty(config).expect("DeviceSelectionConfig always serializes");
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, json)?;
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::types::AudioSource;

    fn device(id: &str, is_default: bool) -> AudioDevice {
        AudioDevice {
            id: id.to_string(),
            name: format!("Device {id}"),
            source: AudioSource::Microphone,
            is_default,
        }
    }

    // Cenário 1 / 2 do checklist: dispositivo padrão pré-selecionado quando não há
    // seleção persistida (a mesma função serve para entrada e saída — a categoria não
    // afeta a lógica de resolução, só a lista de dispositivos passada).
    #[test]
    fn default_device_is_preselected_when_no_persisted_selection_exists() {
        let devices = vec![device("mic-1", false), device("mic-2", true)];
        let resolved = resolve_device(&devices, None).unwrap();
        assert_eq!(resolved.device_id, "mic-2");
        assert!(resolved.is_windows_default);
        assert_eq!(resolved.source, ResolutionSource::WindowsDefault);
    }

    // Cenário 3.
    #[test]
    fn falls_back_to_first_available_device_when_no_default_exists() {
        let devices = vec![device("mic-1", false), device("mic-2", false)];
        let resolved = resolve_device(&devices, None).unwrap();
        assert_eq!(resolved.device_id, "mic-1");
        assert_eq!(resolved.source, ResolutionSource::FirstAvailableFallback);
    }

    // Cenário 4.
    #[test]
    fn returns_none_when_no_device_is_available() {
        assert_eq!(resolve_device(&[], None), None);
    }

    // Cenário 13.
    #[test]
    fn restores_persisted_selection_over_the_current_default() {
        let devices = vec![device("mic-1", true), device("mic-2", false)];
        let resolved = resolve_device(&devices, Some("mic-2")).unwrap();
        assert_eq!(resolved.device_id, "mic-2");
        assert_eq!(resolved.source, ResolutionSource::Persisted);
    }

    // Cenário 14.
    #[test]
    fn falls_back_to_default_when_persisted_device_no_longer_exists() {
        let devices = vec![device("mic-1", true)];
        let resolved = resolve_device(&devices, Some("ghost-device")).unwrap();
        assert_eq!(resolved.device_id, "mic-1");
        assert_eq!(resolved.source, ResolutionSource::WindowsDefault);
    }

    // Cenário 15: a troca do padrão do Windows em segundo plano nunca sobrescreve
    // silenciosamente uma seleção persistida que ainda existe.
    #[test]
    fn keeps_persisted_selection_even_when_a_different_device_becomes_the_new_default() {
        let devices = vec![
            device("mic-old-default", false),
            device("mic-new-default", true),
        ];
        let resolved = resolve_device(&devices, Some("mic-old-default")).unwrap();
        assert_eq!(resolved.device_id, "mic-old-default");
        assert_eq!(resolved.source, ResolutionSource::Persisted);
        assert!(!resolved.is_windows_default);
    }

    fn temp_config_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "helppye-device-selection-test-{name}-{}.json",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn missing_config_file_loads_as_default() {
        let path = temp_config_path("missing");
        assert_eq!(load(&path), DeviceSelectionConfig::default());
    }

    #[test]
    fn saved_selection_round_trips_through_load() {
        let path = temp_config_path("roundtrip");
        let config = DeviceSelectionConfig {
            input_device_id: Some("mic-1".into()),
            output_device_id: Some("speaker-1".into()),
        };
        save(&path, &config).unwrap();
        assert_eq!(load(&path), config);
        std::fs::remove_file(&path).ok();
    }
}
