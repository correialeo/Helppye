//! Definição declarativa de um caso de benchmark.
//!
//! O manifesto é um JSON versionável; os arquivos de áudio a que ele aponta, não. Isso é
//! deliberado: a transcrição esperada e o vocabulário técnico de um caso são a parte que
//! precisa de revisão em code review ("esse é mesmo o texto certo?"), enquanto o `.wav` é
//! gravação de fala — muitas vezes de outra pessoa — que não deve entrar num repositório
//! público.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::audio::types::AudioSource;
use crate::transcription::settings::LanguageCode;

#[derive(Debug, thiserror::Error)]
pub enum FixtureError {
    #[error("não foi possível ler o manifesto {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("manifesto inválido: {0}")]
    Invalid(#[from] serde_json::Error),
    #[error("fixture '{id}' aponta para um áudio inexistente: {path}")]
    MissingAudio { id: String, path: String },
    #[error("fixture '{0}' tem id duplicado")]
    DuplicateId(String),
}

/// Um caso: um arquivo de áudio e tudo que se espera dele.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenchmarkFixture {
    pub id: String,
    /// Caminho do áudio, relativo ao diretório do manifesto.
    pub audio: PathBuf,
    /// Transcrição de referência, escrita por uma pessoa. É o denominador do WER.
    pub expected_transcript: String,
    /// Termos técnicos que **têm** que sobreviver ao pipeline. Medidos separadamente do WER
    /// porque errar "RabbitMQ" e errar um artigo não são o mesmo tipo de erro: o primeiro
    /// muda o que o modelo de resposta entende, o segundo não.
    #[serde(default)]
    pub technical_vocabulary: Vec<String>,
    /// Qual lado da conversa este áudio representa. Nunca é inferido — a distinção
    /// microfone/saída de sistema é o que define quem falou.
    pub source: AudioSource,
    #[serde(default)]
    pub language: LanguageCode,
    /// Nota livre para quem lê o relatório ("fala rápida com sotaque", "ruído de fundo").
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FixtureManifest {
    pub fixtures: Vec<BenchmarkFixture>,
}

impl FixtureManifest {
    /// Carrega e **valida**: ids únicos e áudios existentes. Falhar aqui é melhor que
    /// produzir um relatório com metade dos casos silenciosamente ausentes — um benchmark
    /// incompleto que parece completo leva à conclusão errada.
    pub fn load(path: &Path) -> Result<Self, FixtureError> {
        let contents = std::fs::read_to_string(path).map_err(|source| FixtureError::Io {
            path: path.display().to_string(),
            source,
        })?;
        let manifest: FixtureManifest = serde_json::from_str(&contents)?;
        let base = path.parent().unwrap_or(Path::new("."));

        let mut seen = Vec::new();
        for fixture in &manifest.fixtures {
            if seen.contains(&fixture.id) {
                return Err(FixtureError::DuplicateId(fixture.id.clone()));
            }
            seen.push(fixture.id.clone());

            let audio = base.join(&fixture.audio);
            if !audio.is_file() {
                return Err(FixtureError::MissingAudio {
                    id: fixture.id.clone(),
                    path: audio.display().to_string(),
                });
            }
        }

        Ok(manifest)
    }

    /// Resolve o caminho do áudio de um fixture relativo ao manifesto.
    pub fn audio_path(manifest_path: &Path, fixture: &BenchmarkFixture) -> PathBuf {
        manifest_path
            .parent()
            .unwrap_or(Path::new("."))
            .join(&fixture.audio)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "helppye-benchmark-fixtures-{name}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_manifest(dir: &Path, json: &str) -> PathBuf {
        let path = dir.join("fixtures.json");
        std::fs::write(&path, json).unwrap();
        path
    }

    #[test]
    fn loads_a_manifest_and_resolves_audio_relative_to_it() {
        let dir = temp_dir("ok");
        std::fs::write(dir.join("a.wav"), b"fake").unwrap();
        let path = write_manifest(
            &dir,
            r#"{"fixtures":[{"id":"a","audio":"a.wav","expected_transcript":"olá",
                "technical_vocabulary":["DDD"],"source":"system_output"}]}"#,
        );

        let manifest = FixtureManifest::load(&path).unwrap();
        assert_eq!(manifest.fixtures.len(), 1);
        let fixture = &manifest.fixtures[0];
        assert_eq!(fixture.source, AudioSource::SystemOutput);
        assert_eq!(fixture.language, LanguageCode::default());
        assert_eq!(
            FixtureManifest::audio_path(&path, fixture),
            dir.join("a.wav")
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_audio_fails_loudly_instead_of_being_skipped() {
        let dir = temp_dir("missing");
        let path = write_manifest(
            &dir,
            r#"{"fixtures":[{"id":"a","audio":"nao-existe.wav",
                "expected_transcript":"olá","source":"microphone"}]}"#,
        );
        assert!(matches!(
            FixtureManifest::load(&path),
            Err(FixtureError::MissingAudio { .. })
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        let dir = temp_dir("dupe");
        std::fs::write(dir.join("a.wav"), b"fake").unwrap();
        let path = write_manifest(
            &dir,
            r#"{"fixtures":[
                {"id":"a","audio":"a.wav","expected_transcript":"x","source":"microphone"},
                {"id":"a","audio":"a.wav","expected_transcript":"y","source":"microphone"}
            ]}"#,
        );
        assert!(matches!(
            FixtureManifest::load(&path),
            Err(FixtureError::DuplicateId(_))
        ));
        std::fs::remove_dir_all(&dir).ok();
    }
}
