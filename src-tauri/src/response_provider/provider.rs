//! Abstração de provedor de sugestão de resposta. Cada implementação recebe um
//! `ResponseRequest` já montado (system prompt + histórico limitado) e devolve um stream
//! de deltas de texto — nunca a resposta inteira de uma vez, para manter a latência de
//! ponta a ponta baixa.

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseRole {
    System,
    User,
}

impl ResponseRole {
    pub fn as_str(self) -> &'static str {
        match self {
            ResponseRole::System => "system",
            ResponseRole::User => "user",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResponseMessage {
    pub role: ResponseRole,
    pub content: String,
}

/// Serializa no formato `{role, content}` usado por Ollama e por qualquer backend
/// compatível com a API de chat da OpenAI.
pub fn to_chat_json(message: &ResponseMessage) -> serde_json::Value {
    serde_json::json!({ "role": message.role.as_str(), "content": message.content })
}

#[derive(Debug, Clone)]
pub struct ResponseRequest {
    pub messages: Vec<ResponseMessage>,
    pub max_output_tokens: u32,
}

#[derive(Debug, Clone)]
pub enum ResponseChunk {
    Delta(String),
    Done,
}

#[derive(Debug, Error)]
pub enum ResponseProviderError {
    #[error("falha de rede: {0}")]
    Network(String),
    #[error("resposta inválida do provedor: {0}")]
    InvalidResponse(String),
    #[error("credencial ausente ou inválida: {0}")]
    Credential(String),
    #[error("provedor retornou erro: {0}")]
    Provider(String),
}

pub type ResponseStream = BoxStream<'static, Result<ResponseChunk, ResponseProviderError>>;

#[async_trait]
pub trait ResponseProvider: Send + Sync {
    fn provider_name(&self) -> &'static str;

    async fn stream_reply(
        &self,
        request: ResponseRequest,
    ) -> Result<ResponseStream, ResponseProviderError>;
}
