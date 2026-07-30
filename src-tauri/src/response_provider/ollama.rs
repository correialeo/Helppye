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
/// Mantém o modelo carregado na GPU/CPU entre chamadas (ver `docs/response-suggestion.md`,
/// seção de latência) — sem isso, o Ollama descarrega o modelo por padrão após um curto
/// período ocioso, e a chamada seguinte paga o custo de recarregá-lo (segundos) além da
/// própria inferência. Configurável via `ResponseProviderConfig::ollama_keep_alive`.
pub const DEFAULT_KEEP_ALIVE: &str = "10m";

pub struct OllamaProvider {
    client: reqwest::Client,
    base_url: String,
    model: String,
    keep_alive: Option<String>,
}

impl OllamaProvider {
    pub fn new(base_url: Option<String>, model: String, keep_alive: Option<String>) -> Self {
        OllamaProvider {
            // Uma única instância reutilizada por toda a vida do provider (que por sua vez
            // é reconstruído só quando a configuração muda, ver `engine::build_provider`) —
            // recriar o client a cada chamada jogaria fora o pool de conexões HTTP do
            // reqwest, somando round-trips de handshake TCP/TLS à latência de cada geração.
            client: reqwest::Client::new(),
            base_url: base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            model,
            keep_alive,
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
        let mut body = serde_json::json!({
            "model": self.model,
            "messages": request.messages.iter().map(to_chat_json).collect::<Vec<_>>(),
            "stream": true,
            // `num_predict` é o equivalente do Ollama a `max_output_tokens`: sem ele, a
            // geração não tinha teto nenhum e podia continuar bem além do necessário para
            // uma sugestão de resposta curta, inflando a latência percebida.
            "options": {
                "temperature": request.temperature,
                "num_predict": request.max_output_tokens,
            },
            // Desliga o modo de raciocínio estendido em modelos híbridos (ex.: qwen3) —
            // sem isso, o modelo pode gastar segundos "pensando" antes do primeiro token
            // visível, e nada aqui depende de parsing de tags de raciocínio para
            // compensar. Ignorado silenciosamente por modelos/versões do Ollama que não
            // suportam o campo.
            "think": false,
        });
        if let Some(keep_alive) = &self.keep_alive {
            body["keep_alive"] = serde_json::Value::String(keep_alive.clone());
        }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::response_provider::provider::ResponseRequest;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Minimal hand-rolled HTTP/1.1 server: accepts one connection, reads headers + body
    /// (using `Content-Length`), replies with a canned NDJSON stream, and returns the raw
    /// request body it received so the test can assert on it. No mocking crate involved —
    /// this is the one place the codebase needs to see the *exact bytes* `OllamaProvider`
    /// puts on the wire (`keep_alive`, `options`, `think`), which nothing at the
    /// `ResponseRequest`/`ResponseChunk` level of abstraction can verify.
    async fn serve_one_request_and_capture_body(listener: TcpListener) -> String {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = stream.read(&mut chunk).await.unwrap();
            assert!(
                n > 0,
                "connection closed before a full request was received"
            );
            buf.extend_from_slice(&chunk[..n]);
            let Some(header_end) = find_double_crlf(&buf) else {
                continue;
            };
            let headers = String::from_utf8_lossy(&buf[..header_end]).to_string();
            let content_length: usize = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().ok())
                        .flatten()
                })
                .unwrap_or(0);
            let body_start = header_end + 4;
            while buf.len() < body_start + content_length {
                let n = stream.read(&mut chunk).await.unwrap();
                buf.extend_from_slice(&chunk[..n]);
            }
            let body =
                String::from_utf8_lossy(&buf[body_start..body_start + content_length]).to_string();

            let ndjson = "{\"message\":{\"content\":\"oi\"},\"done\":false}\n{\"done\":true}\n";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                ndjson.len(),
                ndjson
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            stream.shutdown().await.ok();
            return body;
        }
    }

    fn find_double_crlf(buf: &[u8]) -> Option<usize> {
        buf.windows(4).position(|w| w == b"\r\n\r\n")
    }

    #[tokio::test]
    async fn request_body_carries_keep_alive_options_and_think_false() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(serve_one_request_and_capture_body(listener));

        let provider = OllamaProvider::new(
            Some(format!("http://{addr}")),
            "qwen3:8b".to_string(),
            Some("10m".to_string()),
        );
        let request = ResponseRequest {
            messages: vec![],
            max_output_tokens: 160,
            temperature: 0.2,
        };
        let (mut stream, meta) = provider.stream_reply(request).await.unwrap();
        assert_eq!(meta.http_status, 200);
        while stream.next().await.is_some() {}

        let body: serde_json::Value = serde_json::from_str(&server.await.unwrap()).unwrap();
        assert_eq!(body["model"], "qwen3:8b");
        assert_eq!(body["keep_alive"], "10m");
        assert_eq!(body["think"], false);
        assert_eq!(body["options"]["num_predict"], 160);
        let temperature = body["options"]["temperature"].as_f64().unwrap();
        assert!(
            (temperature - 0.2).abs() < 1e-6,
            "temperature should round-trip as ~0.2 (f32 widened to f64), got {temperature}"
        );
    }

    /// Ignorado por padrão: precisa de um Ollama de verdade em `localhost:11434` com o
    /// modelo `qwen3:8b` puxado. Não faz parte de `cargo test` (que precisa continuar
    /// hermético/determinístico), mas é a forma de coletar números reais de latência
    /// contra `request_to_first_http_chunk_ms`/`request_to_first_visible_token_ms` sem
    /// fabricar dados — rode manualmente com:
    /// `cargo test --target x86_64-unknown-linux-gnu -- --ignored measure_real_ollama`.
    #[tokio::test]
    #[ignore = "requires a real Ollama running locally with qwen3:8b pulled"]
    async fn measure_real_ollama_latency_with_the_production_request_shape() {
        use crate::response_provider::provider::{ResponseMessage, ResponseRole};
        use std::time::Instant;

        let provider = OllamaProvider::new(None, "qwen3:8b".to_string(), Some("10m".to_string()));
        let request = ResponseRequest {
            messages: vec![
                ResponseMessage {
                    role: ResponseRole::System,
                    content: "Você é um copiloto que ajuda o usuário durante uma reunião ao vivo. \
                        Se a fala mais recente não for uma pergunta ou pedido que exija resposta \
                        do usuário, responda apenas com o texto exato [SKIP] e nada mais. Caso \
                        contrário, responda de forma breve, direta e natural."
                        .to_string(),
                },
                ResponseMessage {
                    role: ResponseRole::User,
                    content: "Fala mais recente de Outra pessoa: Em qual situação você usaria \
                        microsserviços em vez de um monolito?"
                        .to_string(),
                },
            ],
            max_output_tokens: 160,
            temperature: 0.2,
        };

        let request_started_at = Instant::now();
        let (mut stream, meta) = provider
            .stream_reply(request)
            .await
            .expect("failed to reach a local Ollama at localhost:11434 with qwen3:8b pulled");
        assert_eq!(meta.http_status, 200);

        let mut first_chunk_at = None;
        let mut first_text_at = None;
        let mut full_text = String::new();
        while let Some(item) = stream.next().await {
            if first_chunk_at.is_none() {
                first_chunk_at = Some(Instant::now());
            }
            if let Ok(ResponseChunk::Delta(text)) = item {
                if first_text_at.is_none() && !text.is_empty() {
                    first_text_at = Some(Instant::now());
                }
                full_text.push_str(&text);
            }
        }

        println!(
            "request_to_first_http_chunk_ms={:?} request_to_first_text_ms={:?} total_ms={:?} text={:?}",
            first_chunk_at.map(|t| t.duration_since(request_started_at)),
            first_text_at.map(|t| t.duration_since(request_started_at)),
            request_started_at.elapsed(),
            full_text,
        );
        assert!(
            !full_text.trim().is_empty(),
            "expected real content, not a [SKIP]"
        );
    }

    #[tokio::test]
    async fn no_keep_alive_configured_omits_the_field_entirely() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(serve_one_request_and_capture_body(listener));

        let provider =
            OllamaProvider::new(Some(format!("http://{addr}")), "llama3.1".to_string(), None);
        let request = ResponseRequest {
            messages: vec![],
            max_output_tokens: 160,
            temperature: 0.2,
        };
        let (mut stream, _meta) = provider.stream_reply(request).await.unwrap();
        while stream.next().await.is_some() {}

        let body: serde_json::Value = serde_json::from_str(&server.await.unwrap()).unwrap();
        assert!(
            body.get("keep_alive").is_none(),
            "keep_alive should be entirely absent, not null, when not configured"
        );
    }
}
