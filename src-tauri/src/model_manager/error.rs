use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq)]
pub enum ModelManagerError {
    #[error("failed to resolve application data directory: {0}")]
    DataDirUnavailable(String),
    #[error("failed to create models directory: {0}")]
    DirectoryCreationFailed(String),
    #[error("network error while downloading model: {0}")]
    Network(String),
    #[error("server returned an unexpected response: {0}")]
    InvalidResponse(String),
    #[error("failed writing model file to disk: {0}")]
    Disk(String),
    #[error("download cancelled")]
    Cancelled,
    #[error("downloaded file size ({actual}) does not match expected size ({expected})")]
    SizeMismatch { actual: u64, expected: u64 },
    #[error("checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },
    #[error("model failed to load: {0}")]
    LoadFailed(String),
    #[error("selected file is not a valid model: {0}")]
    InvalidCustomModel(String),
    #[error("managed model is not installed: {0}")]
    ManagedModelNotInstalled(String),
}
