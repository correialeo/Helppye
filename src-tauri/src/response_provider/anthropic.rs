//! Provedor Anthropic nativo (Messages API). Autenticação via `x-api-key`, streaming via
//! Server-Sent Events com eventos nomeados (`content_block_delta`, `message_stop`, etc.),
//! diferente do formato usado por OpenAI/DeepSeek.

use async_trait::async_trait;
use futures_util::stream::StreamExt;
use serde::Deserialize;

use super::net::{line_stream, sse_data_payloads};
use super::provider::{
    ResponseChunk, ResponseMessage, ResponseProvider, ResponseProviderError, ResponseRequest,
    ResponseRole, ResponseStream, ResponseStreamMeta,
};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct AnthropicProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl AnthropicProvider {
    pub fn new(api_key: String, model: String, base_url: Option<String>) -> Self {
        AnthropicProvider {
            client: reqwest::Client::new(),
            base_url: base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            api_key,
            model,
        }
    }
}

fn split_messages(messages: &[ResponseMessage]) -> (Option<String>, Vec<serde_json::Value>) {
    let mut system_parts = Vec::new();
    let mut chat = Vec::new();
    for message in messages {
        match message.role {
            ResponseRole::System => system_parts.push(message.content.clone()),
            ResponseRole::User => {
                chat.push(serde_json::json!({ "role": "user", "content": message.content }))
            }
        }
    }
    let system = (!system_parts.is_empty()).then(|| system_parts.join("\n\n"));
    (system, chat)
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum AnthropicEvent {
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta { delta: AnthropicDelta },
    #[serde(rename = "message_stop")]
    MessageStop,
    #[serde(rename = "error")]
    Error { error: AnthropicErrorBody },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum AnthropicDelta {
    #[serde(rename = "text_delta")]
    TextDelta { text: String },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct AnthropicErrorBody {
    message: String,
}

#[async_trait]
impl ResponseProvider for AnthropicProvider {
    fn provider_name(&self) -> &'static str {
        "anthropic"
    }

    async fn stream_reply(
        &self,
        request: ResponseRequest,
    ) -> Result<(ResponseStream, ResponseStreamMeta), ResponseProviderError> {
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let (system, chat_messages) = split_messages(&request.messages);
        let mut body = serde_json::json!({
            "model": self.model,
            "max_tokens": request.max_output_tokens,
            "messages": chat_messages,
            "stream": true,
        });
        if let Some(system) = system {
            body["system"] = serde_json::Value::String(system);
        }

        let response = self
            .client
            .post(url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&body)
            .send()
            .await
            .map_err(|e| ResponseProviderError::Network(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(ResponseProviderError::Provider(format!("{status}: {text}")));
        }

        let meta = ResponseStreamMeta {
            http_status: response.status().as_u16(),
        };
        let bytes = response.bytes_stream().boxed();
        let payloads = sse_data_payloads(line_stream(bytes));

        let stream: ResponseStream = Box::pin(payloads.filter_map(|payload| async move {
            let payload = match payload {
                Ok(p) => p,
                Err(e) => return Some(Err(ResponseProviderError::Network(e.to_string()))),
            };
            if payload.is_empty() {
                return None;
            }
            let event: AnthropicEvent = match serde_json::from_str(&payload) {
                Ok(e) => e,
                Err(e) => return Some(Err(ResponseProviderError::InvalidResponse(e.to_string()))),
            };
            match event {
                AnthropicEvent::ContentBlockDelta {
                    delta: AnthropicDelta::TextDelta { text },
                } => {
                    if text.is_empty() {
                        None
                    } else {
                        Some(Ok(ResponseChunk::Delta(text)))
                    }
                }
                AnthropicEvent::ContentBlockDelta {
                    delta: AnthropicDelta::Other,
                } => None,
                AnthropicEvent::MessageStop => Some(Ok(ResponseChunk::Done)),
                AnthropicEvent::Error { error } => {
                    Some(Err(ResponseProviderError::Provider(error.message)))
                }
                AnthropicEvent::Other => None,
            }
        }));

        Ok((stream, meta))
    }
}
