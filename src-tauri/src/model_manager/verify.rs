//! Verificação de integridade: um arquivo nunca é considerado "instalado" apenas por
//! existir — tamanho e checksum SHA-256 precisam bater com o que está versionado em
//! `catalog::ModelDefinition`. Streaming em blocos de 1 MiB para não carregar o modelo
//! inteiro (~140 MB) na memória de uma vez.

use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::model_manager::error::ModelManagerError;

const CHUNK_SIZE: usize = 1024 * 1024;

pub fn compute_sha256(path: &Path) -> Result<String, ModelManagerError> {
    let mut file = std::fs::File::open(path).map_err(|e| ModelManagerError::Disk(e.to_string()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK_SIZE];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| ModelManagerError::Disk(e.to_string()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Verifica tamanho e checksum de `path` contra os valores esperados. `Ok(())` só é
/// retornado se ambos baterem exatamente.
pub fn verify_file(
    path: &Path,
    expected_sha256: &str,
    expected_size: u64,
) -> Result<(), ModelManagerError> {
    let metadata = std::fs::metadata(path).map_err(|e| ModelManagerError::Disk(e.to_string()))?;
    if metadata.len() != expected_size {
        return Err(ModelManagerError::SizeMismatch {
            actual: metadata.len(),
            expected: expected_size,
        });
    }
    let actual_hash = compute_sha256(path)?;
    if actual_hash != expected_sha256 {
        return Err(ModelManagerError::ChecksumMismatch {
            expected: expected_sha256.to_string(),
            actual: actual_hash,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp_file(name: &str, contents: &[u8]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "helppye-verify-test-{name}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(contents).unwrap();
        path
    }

    #[test]
    fn valid_checksum_passes_verification() {
        let contents = b"hello helppye model bytes";
        let path = write_temp_file("valid", contents);
        let expected_hash = compute_sha256(&path).unwrap();

        let result = verify_file(&path, &expected_hash, contents.len() as u64);

        assert!(result.is_ok());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn invalid_checksum_fails_verification() {
        let contents = b"hello helppye model bytes";
        let path = write_temp_file("invalid", contents);

        let result = verify_file(
            &path,
            "0000000000000000000000000000000000000000000000000000000000000",
            contents.len() as u64,
        );

        assert!(matches!(
            result,
            Err(ModelManagerError::SizeMismatch { .. })
                | Err(ModelManagerError::ChecksumMismatch { .. })
        ));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn size_mismatch_is_reported_distinctly_from_checksum_mismatch() {
        let contents = b"short";
        let path = write_temp_file("size-mismatch", contents);
        let expected_hash = compute_sha256(&path).unwrap();

        let result = verify_file(&path, &expected_hash, 9999);

        assert!(matches!(
            result,
            Err(ModelManagerError::SizeMismatch {
                actual: 5,
                expected: 9999
            })
        ));
        std::fs::remove_file(&path).ok();
    }
}
