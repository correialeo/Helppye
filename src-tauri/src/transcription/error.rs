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
    /// Áudio entregue a uma sessão de transcrição já encerrada (`finish`/`cancel`). É um
    /// erro, e não um no-op silencioso: aceitar áudio depois do encerramento é exatamente o
    /// vazamento entre sessões que o ciclo de vida existe para impedir.
    #[error("transcription session already closed")]
    SessionClosed,
    /// Áudio da fonte errada. Uma sessão de transcrição pertence a **uma** fonte; misturar
    /// microfone com saída de sistema faria a fala do usuário ser atribuída à outra pessoa.
    #[error("audio source mismatch: session is for {expected:?}, received {received:?}")]
    SourceMismatch {
        expected: crate::audio::types::AudioSource,
        received: crate::audio::types::AudioSource,
    },
    /// Provider selecionado não existe no registry, ou existe mas não está implementado
    /// nesta build. Nunca cair em outro provider silenciosamente.
    #[error("transcription provider unavailable: {0}")]
    ProviderUnavailable(String),
    /// Provider de nuvem sem credencial configurada no keychain.
    #[error("transcription provider requires credentials: {0}")]
    MissingCredentials(String),
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
