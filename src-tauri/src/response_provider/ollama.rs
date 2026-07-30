//! Provedor Ollama nativo: `/api/chat` com `stream: true`, streaming NDJSON (uma linha
//! JSON completa por chunk, sem framing SSE). Local por padrão — sem API key.

use async_trait::async_trait;
use futures_util::stream::StreamExt;
use serde::Deserialize;

use super::net::line_stream;
use super::provider::{
    to_chat_json, ResponseChunk, ResponseProvider, ResponseProviderError, ResponseRequest,
    ResponseStream, ResponseStreamMeta,
};

const DEFAULT_BASE_URL: &str = "http://localhost:11434";

pub struct OllamaProvider {
    client: reqwest::Client,
    base_url: String,
    model: String,
}

impl OllamaProvider {
    pub fn new(base_url: Option<String>, model: String) -> Self {
        OllamaProvider {
            client: reqwest::Client::new(),
            base_url: base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            model,
        }
    }
}

#[derive(Debug, Deserialize)]
struct OllamaStreamLine {
    #[serde(default)]
    message: Option<OllamaMessage>,
    #[serde(default)]
    done: bool,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OllamaMessage {
    #[serde(default)]
    content: String,
}

#[async_trait]
impl ResponseProvider for OllamaProvider {
    fn provider_name(&self) -> &'static str {
        "ollama"
    }

    async fn stream_reply(
        &self,
        request: ResponseRequest,
    ) -> Result<(ResponseStream, ResponseStreamMeta), ResponseProviderError> {
        let url = format!("{}/api/chat", self.base_url.trim_end_matches('/'));
        let body = serde_json::json!({
            "model": self.model,
            "messages": request.messages.iter().map(to_chat_json).collect::<Vec<_>>(),
            "stream": true,
        });

        let response = self
            .client
            .post(url)
            .json(&body)
            .send()
            .await
            .map_err(|e| ResponseProviderError::Network(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(ResponseProviderError::Provider(format!(
                "ollama respondeu {status}: {text}"
            )));
        }

        let meta = ResponseStreamMeta {
            http_status: response.status().as_u16(),
        };
        let bytes = response.bytes_stream().boxed();
        let lines = line_stream(bytes);

        let stream: ResponseStream = Box::pin(lines.filter_map(|line| async move {
            let line = match line {
                Ok(l) => l,
                Err(e) => return Some(Err(ResponseProviderError::Network(e.to_string()))),
            };
            if line.trim().is_empty() {
                return None;
            }
            let parsed: OllamaStreamLine = match serde_json::from_str(&line) {
                Ok(p) => p,
                Err(e) => return Some(Err(ResponseProviderError::InvalidResponse(e.to_string()))),
            };
            if let Some(error) = parsed.error {
                return Some(Err(ResponseProviderError::Provider(error)));
            }
            if parsed.done {
                return Some(Ok(ResponseChunk::Done));
            }
            let content = parsed.message.map(|m| m.content).unwrap_or_default();
            if content.is_empty() {
                None
            } else {
                Some(Ok(ResponseChunk::Delta(content)))
            }
        }));

        Ok((stream, meta))
    }
}
