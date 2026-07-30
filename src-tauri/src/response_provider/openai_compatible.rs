//! Cliente compartilhado para APIs compatíveis com a API de chat da OpenAI: OpenAI e
//! DeepSeek. Streaming via Server-Sent Events, autenticação `Authorization: Bearer`.

use async_trait::async_trait;
use futures_util::stream::StreamExt;
use serde::Deserialize;

use super::net::{line_stream, sse_data_payloads};
use super::provider::{
    to_chat_json, ResponseChunk, ResponseProvider, ResponseProviderError, ResponseRequest,
    ResponseStream, ResponseStreamMeta,
};

const OPENAI_DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const DEEPSEEK_DEFAULT_BASE_URL: &str = "https://api.deepseek.com/v1";

pub struct OpenAiCompatibleProvider {
    client: reqwest::Client,
    provider_name: &'static str,
    base_url: String,
    api_key: String,
    model: String,
}

impl OpenAiCompatibleProvider {
    pub fn openai(api_key: String, model: String, base_url: Option<String>) -> Self {
        OpenAiCompatibleProvider::new(
            "openai",
            base_url.unwrap_or_else(|| OPENAI_DEFAULT_BASE_URL.to_string()),
            api_key,
            model,
        )
    }

    pub fn deepseek(api_key: String, model: String, base_url: Option<String>) -> Self {
        OpenAiCompatibleProvider::new(
            "deepseek",
            base_url.unwrap_or_else(|| DEEPSEEK_DEFAULT_BASE_URL.to_string()),
            api_key,
            model,
        )
    }

    fn new(provider_name: &'static str, base_url: String, api_key: String, model: String) -> Self {
        OpenAiCompatibleProvider {
            client: reqwest::Client::new(),
            provider_name,
            base_url,
            api_key,
            model,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ChatCompletionChunk {
    #[serde(default)]
    choices: Vec<ChatCompletionChoice>,
}

#[derive(Debug, Default, Deserialize)]
struct ChatCompletionChoice {
    #[serde(default)]
    delta: ChatCompletionDelta,
}

#[derive(Debug, Default, Deserialize)]
struct ChatCompletionDelta {
    #[serde(default)]
    content: Option<String>,
}

#[async_trait]
impl ResponseProvider for OpenAiCompatibleProvider {
    fn provider_name(&self) -> &'static str {
        self.provider_name
    }

    async fn stream_reply(
        &self,
        request: ResponseRequest,
    ) -> Result<(ResponseStream, ResponseStreamMeta), ResponseProviderError> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let body = serde_json::json!({
            "model": self.model,
            "messages": request.messages.iter().map(to_chat_json).collect::<Vec<_>>(),
            "max_tokens": request.max_output_tokens,
            "temperature": request.temperature,
            "stream": true,
        });

        let response = self
            .client
            .post(url)
            .bearer_auth(&self.api_key)
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
            if payload == "[DONE]" {
                return Some(Ok(ResponseChunk::Done));
            }
            let chunk: ChatCompletionChunk = match serde_json::from_str(&payload) {
                Ok(c) => c,
                Err(e) => return Some(Err(ResponseProviderError::InvalidResponse(e.to_string()))),
            };
            let content = chunk
                .choices
                .into_iter()
                .next()
                .and_then(|c| c.delta.content)
                .unwrap_or_default();
            if content.is_empty() {
                None
            } else {
                Some(Ok(ResponseChunk::Delta(content)))
            }
        }));

        Ok((stream, meta))
    }
}
