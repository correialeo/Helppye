use thiserror::Error;

#[derive(Debug, Error, Clone)]
pub enum TranscriptionError {
    #[error("no transcription model configured")]
    NotConfigured,
    #[error("model file not found: {0}")]
    ModelNotFound(String),
    #[error("failed to load model: {0}")]
    ModelLoadFailed(String),
    #[error("out of memory loading model")]
    OutOfMemory,
    #[error("invalid model format: {0}")]
    InvalidModelFormat(String),
    #[error("transcription inference failed: {0}")]
    InferenceFailed(String),
    #[error("transcription queue is full, segment dropped")]
    QueueFull,
    #[error("transcription cancelled")]
    Cancelled,
    #[error("internal transcription error: {0}")]
    Internal(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_messages_include_context() {
        let err = TranscriptionError::ModelNotFound("/models/ggml-small.bin".into());
        assert_eq!(
            err.to_string(),
            "model file not found: /models/ggml-small.bin"
        );
    }
}
