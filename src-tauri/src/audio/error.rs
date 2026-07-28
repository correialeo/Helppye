use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq)]
pub enum AudioCaptureError {
    #[error("no matching audio device found")]
    NoDeviceFound,
    #[error("device unavailable: {0}")]
    DeviceUnavailable(String),
    #[error("failed to build audio stream: {0}")]
    StreamBuildFailed(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("capture cancelled")]
    Cancelled,
    #[error("capture already running for this source")]
    AlreadyRunning,
    #[error("internal audio error: {0}")]
    Internal(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_messages_include_context() {
        let err = AudioCaptureError::Unsupported("PipeWire capture not implemented".into());
        assert_eq!(
            err.to_string(),
            "unsupported: PipeWire capture not implemented"
        );
    }
}
