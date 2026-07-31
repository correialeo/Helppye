//! Cliente para qualquer API que fale o dialeto de chat da OpenAI: a própria OpenAI,
//! DeepSeek, LM Studio (local), OpenRouter e endpoints personalizados. Streaming via
//! Server-Sent Events.
//!
//! Este arquivo já era compartilhado entre OpenAI e DeepSeek, mas com `base_url` e
//! autenticação fixos em código. O que mudou: `base_url`, modelo, forma de credencial e
//! cabeçalhos extras passam a vir da configuração — o que significa que passam a vir do
//! **usuário**. Três consequências foram tratadas aqui, e nenhuma delas é opcional:
//!
//! - A URL é validada e classificada por `endpoint::validate_base_url` antes de virar
//!   destino, e só a forma sanitizada (`esquema://host:porta`) aparece em log ou em
//!   mensagem de erro.
//! - A credencial mora no keychain e nunca entra em `Debug`, log ou erro. Erro de HTTP
//!   inclui corpo da resposta do provedor, então o corpo é truncado — alguns serviços
//!   ecoam a chave enviada dentro da mensagem de erro.
//! - Cabeçalhos personalizados não podem sobrescrever os que o app monta (`Authorization`,
//!   `Host`, `Content-Type`), e seus valores são marcados como sensíveis.

use async_trait::async_trait;
use futures_util::stream::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};

use super::endpoint::{
    build_client, build_custom_headers, classify_request_error, validate_base_url,
    ValidatedEndpoint,
};
use super::net::{line_stream, sse_data_payloads};
use super::provider::{
    to_chat_json, ResponseChunk, ResponseProvider, ResponseProviderCapabilities,
    ResponseProviderError, ResponseProviderId, ResponseRequest, ResponseStream, ResponseStreamMeta,
};

pub const OPENAI_DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
pub const DEEPSEEK_DEFAULT_BASE_URL: &str = "https://api.deepseek.com/v1";
/// Porta padrão do servidor local do LM Studio.
pub const LM_STUDIO_DEFAULT_BASE_URL: &str = "http://localhost:1234/v1";
pub const OPENROUTER_DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";

/// Quanto do corpo de uma resposta de erro entra na mensagem. Sem teto, um provedor que
/// devolve HTML de proxy despeja uma página inteira no log; e um punhado de gateways ecoa
/// o cabeçalho recebido — incluindo a credencial — dentro do JSON de erro.
const MAX_ERROR_BODY_CHARS: usize = 300;

/// Como a credencial viaja. A distinção existe porque as duas convenções em uso real não
/// são intercambiáveis: quem espera `Authorization: Bearer` ignora `api-key`, e vice-versa.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialMode {
    /// Nenhuma credencial é enviada. É o caso do LM Studio e de qualquer servidor local
    /// que não exige autenticação — mandar um `Authorization` vazio para ele só produz
    /// 401 confuso.
    None,
    /// Cabeçalho `api-key: <valor>`. Convenção do Azure OpenAI e de vários gateways
    /// corporativos.
    ApiKey,
    /// Cabeçalho `Authorization: Bearer <valor>`. Convenção da OpenAI, DeepSeek e
    /// OpenRouter — o padrão.
    #[default]
    BearerToken,
}

impl CredentialMode {
    pub fn requires_credential(self) -> bool {
        !matches!(self, CredentialMode::None)
    }
}

pub struct OpenAiCompatibleProvider {
    client: reqwest::Client,
    id: ResponseProviderId,
    endpoint: ValidatedEndpoint,
    /// Nunca aparece em `Debug` (a struct não deriva `Debug` de propósito), em log ou em
    /// mensagem de erro. `None` quando `credential_mode` é `None`.
    credential: Option<String>,
    credential_mode: CredentialMode,
    model: String,
}

/// Tudo que define uma instalação compatível com a API da OpenAI. Struct em vez de seis
/// argumentos posicionais porque `base_url`, credencial e modelo são todos `String` — uma
/// troca acidental de ordem entre credencial e modelo mandaria a chave como nome de modelo
/// para o provedor.
pub struct OpenAiCompatibleSettings {
    pub id: ResponseProviderId,
    pub base_url: String,
    pub model: String,
    pub credential: Option<String>,
    pub credential_mode: CredentialMode,
    pub custom_headers: Vec<(String, String)>,
}

impl OpenAiCompatibleProvider {
    pub fn new(settings: OpenAiCompatibleSettings) -> Result<Self, ResponseProviderError> {
        let endpoint = validate_base_url(&settings.base_url)
            .map_err(|e| ResponseProviderError::InvalidResponse(e.to_string()))?;

        if settings.credential_mode.requires_credential()
            && settings
                .credential
                .as_ref()
                .map(|c| c.trim().is_empty())
                .unwrap_or(true)
        {
            return Err(ResponseProviderError::Credential(format!(
                "{} exige credencial, mas nenhuma foi configurada",
                settings.id.display_name()
            )));
        }

        let headers = build_custom_headers(&settings.custom_headers)
            .map_err(|e| ResponseProviderError::InvalidResponse(e.to_string()))?;
        let client =
            build_client(headers).map_err(|e| ResponseProviderError::Network(e.to_string()))?;

        // Uma linha de log por construção, com a forma sanitizada: é o que permite
        // responder "para onde meu áudio está indo" sem nunca imprimir credencial,
        // caminho ou query.
        tracing::info!(
            provider = settings.id.as_str(),
            endpoint = %endpoint.sanitized(),
            classification = ?endpoint.classification(),
            leaves_machine = endpoint.classification().leaves_machine(),
            credential_mode = ?settings.credential_mode,
            custom_header_count = settings.custom_headers.len(),
            "provedor de resposta compatível com a API da OpenAI configurado"
        );

        Ok(OpenAiCompatibleProvider {
            client,
            id: settings.id,
            endpoint,
            credential: settings.credential,
            credential_mode: settings.credential_mode,
            model: settings.model,
        })
    }

    pub fn openai(
        api_key: String,
        model: String,
        base_url: Option<String>,
    ) -> Result<Self, ResponseProviderError> {
        OpenAiCompatibleProvider::new(OpenAiCompatibleSettings {
            id: ResponseProviderId::OpenAi,
            base_url: base_url.unwrap_or_else(|| OPENAI_DEFAULT_BASE_URL.to_string()),
            model,
            credential: Some(api_key),
            credential_mode: CredentialMode::BearerToken,
            custom_headers: Vec::new(),
        })
    }

    pub fn deepseek(
        api_key: String,
        model: String,
        base_url: Option<String>,
    ) -> Result<Self, ResponseProviderError> {
        OpenAiCompatibleProvider::new(OpenAiCompatibleSettings {
            id: ResponseProviderId::DeepSeek,
            base_url: base_url.unwrap_or_else(|| DEEPSEEK_DEFAULT_BASE_URL.to_string()),
            model,
            credential: Some(api_key),
            credential_mode: CredentialMode::BearerToken,
            custom_headers: Vec::new(),
        })
    }

    /// LM Studio roda na máquina do usuário e, por padrão, sem autenticação — é o caminho
    /// "nuvem nenhuma" para quem quer um modelo maior que o do Ollama sem sair do local.
    pub fn lm_studio(
        model: String,
        base_url: Option<String>,
        credential: Option<String>,
    ) -> Result<Self, ResponseProviderError> {
        let credential_mode = if credential.is_some() {
            CredentialMode::BearerToken
        } else {
            CredentialMode::None
        };
        OpenAiCompatibleProvider::new(OpenAiCompatibleSettings {
            id: ResponseProviderId::LmStudio,
            base_url: base_url.unwrap_or_else(|| LM_STUDIO_DEFAULT_BASE_URL.to_string()),
            model,
            credential,
            credential_mode,
            custom_headers: Vec::new(),
        })
    }

    pub fn openrouter(
        api_key: String,
        model: String,
        base_url: Option<String>,
    ) -> Result<Self, ResponseProviderError> {
        OpenAiCompatibleProvider::new(OpenAiCompatibleSettings {
            id: ResponseProviderId::OpenRouter,
            base_url: base_url.unwrap_or_else(|| OPENROUTER_DEFAULT_BASE_URL.to_string()),
            model,
            credential: Some(api_key),
            credential_mode: CredentialMode::BearerToken,
            custom_headers: Vec::new(),
        })
    }

    /// Endpoint totalmente definido pelo usuário. Sem `base_url` padrão: um destino
    /// personalizado sem URL não tem para onde cair de volta que não seja adivinhar.
    pub fn custom(
        base_url: String,
        model: String,
        credential: Option<String>,
        credential_mode: CredentialMode,
        custom_headers: Vec<(String, String)>,
    ) -> Result<Self, ResponseProviderError> {
        OpenAiCompatibleProvider::new(OpenAiCompatibleSettings {
            id: ResponseProviderId::CustomOpenAiCompatible,
            base_url,
            model,
            credential,
            credential_mode,
            custom_headers,
        })
    }

    /// Para onde este provider fala, em forma segura de logar.
    pub fn sanitized_endpoint(&self) -> String {
        self.endpoint.sanitized()
    }

    fn authorization(&self) -> Result<Option<(HeaderName, HeaderValue)>, ResponseProviderError> {
        let Some(credential) = self.credential.as_deref() else {
            return Ok(None);
        };
        let (name, raw) = match self.credential_mode {
            CredentialMode::None => return Ok(None),
            CredentialMode::ApiKey => (HeaderName::from_static("api-key"), credential.to_string()),
            CredentialMode::BearerToken => (
                reqwest::header::AUTHORIZATION,
                format!("Bearer {credential}"),
            ),
        };
        // O erro cita o nome do cabeçalho, jamais o valor: uma chave com caractere de
        // controle (colada com quebra de linha, o caso comum) apareceria inteira no log.
        let mut value = HeaderValue::from_str(&raw).map_err(|_| {
            ResponseProviderError::Credential(format!(
                "credencial inválida para o cabeçalho `{name}`: contém caractere não permitido"
            ))
        })?;
        value.set_sensitive(true);
        Ok(Some((name, value)))
    }
}

fn truncate_error_body(body: &str) -> String {
    let collapsed = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_ERROR_BODY_CHARS {
        return collapsed;
    }
    collapsed
        .chars()
        .take(MAX_ERROR_BODY_CHARS)
        .chain("…".chars())
        .collect()
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
    fn id(&self) -> ResponseProviderId {
        self.id
    }

    fn capabilities(&self) -> ResponseProviderCapabilities {
        ResponseProviderCapabilities {
            local: !self.endpoint.classification().leaves_machine(),
            streaming: true,
            requires_credentials: self.credential_mode.requires_credential(),
            configurable_base_url: true,
            custom_headers: true,
        }
    }

    async fn stream_reply(
        &self,
        request: ResponseRequest,
    ) -> Result<(ResponseStream, ResponseStreamMeta), ResponseProviderError> {
        let url = self.endpoint.endpoint_for("chat/completions");
        let body = serde_json::json!({
            "model": self.model,
            "messages": request.messages.iter().map(to_chat_json).collect::<Vec<_>>(),
            "max_tokens": request.max_output_tokens,
            "temperature": request.temperature,
            "stream": true,
        });

        let mut headers = HeaderMap::new();
        if let Some((name, value)) = self.authorization()? {
            headers.insert(name, value);
        }

        let response = self
            .client
            .post(url)
            .headers(headers)
            .json(&body)
            .send()
            .await
            // `reqwest::Error` faz `Display` da URL completa; um endpoint com chave na
            // query string vazaria por aqui. A forma sanitizada é a única que sai.
            .map_err(|e| classify_request_error(&self.sanitized_endpoint(), e))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(ResponseProviderError::Provider(format!(
                "{status} de {}: {}",
                self.sanitized_endpoint(),
                truncate_error_body(&text)
            )));
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `Result::unwrap_err` exigiria `Debug` em `OpenAiCompatibleProvider`, e a struct
    /// deliberadamente não o implementa: um `Debug` derivado imprimiria a credencial.
    fn expect_error(
        result: Result<OpenAiCompatibleProvider, ResponseProviderError>,
    ) -> ResponseProviderError {
        match result {
            Ok(_) => panic!("esperava erro de configuração, veio um provider"),
            Err(e) => e,
        }
    }

    fn settings(base_url: &str) -> OpenAiCompatibleSettings {
        OpenAiCompatibleSettings {
            id: ResponseProviderId::CustomOpenAiCompatible,
            base_url: base_url.to_string(),
            model: "modelo".to_string(),
            credential: Some("sk-chave-secreta".to_string()),
            credential_mode: CredentialMode::BearerToken,
            custom_headers: Vec::new(),
        }
    }

    #[test]
    fn rejects_non_http_base_url() {
        let err = expect_error(OpenAiCompatibleProvider::new(settings(
            "file:///etc/passwd",
        )));
        assert!(err.to_string().contains("não é suportado"), "{err}");
    }

    #[test]
    fn rejects_base_url_with_embedded_credentials() {
        let err = expect_error(OpenAiCompatibleProvider::new(settings(
            "https://user:pass@example.com/v1",
        )));
        assert!(err.to_string().contains("credencial embutida"), "{err}");
    }

    #[test]
    fn rejects_reserved_custom_headers() {
        let mut s = settings("https://example.com/v1");
        s.custom_headers = vec![("Authorization".to_string(), "Bearer outro".to_string())];
        let err = expect_error(OpenAiCompatibleProvider::new(s));
        assert!(err.to_string().contains("reservado"), "{err}");
    }

    #[test]
    fn missing_credential_is_rejected_when_the_mode_requires_one() {
        let mut s = settings("https://example.com/v1");
        s.credential = None;
        let err = expect_error(OpenAiCompatibleProvider::new(s));
        assert!(matches!(err, ResponseProviderError::Credential(_)), "{err}");

        let mut blank = settings("https://example.com/v1");
        blank.credential = Some("   ".to_string());
        assert!(matches!(
            expect_error(OpenAiCompatibleProvider::new(blank)),
            ResponseProviderError::Credential(_)
        ));
    }

    /// LM Studio local sem autenticação é um caso legítimo, não um erro de configuração.
    #[test]
    fn no_credential_mode_builds_without_a_credential() {
        let provider = OpenAiCompatibleProvider::lm_studio("qwen".to_string(), None, None).unwrap();
        assert_eq!(provider.credential_mode, CredentialMode::None);
        assert!(provider.authorization().unwrap().is_none());
        assert_eq!(provider.sanitized_endpoint(), "http://localhost:1234");
    }

    #[test]
    fn bearer_and_api_key_modes_use_different_headers() {
        let bearer = OpenAiCompatibleProvider::new(settings("https://example.com/v1")).unwrap();
        let (name, _) = bearer.authorization().unwrap().unwrap();
        assert_eq!(name, reqwest::header::AUTHORIZATION);

        let mut s = settings("https://example.com/v1");
        s.credential_mode = CredentialMode::ApiKey;
        let api_key = OpenAiCompatibleProvider::new(s).unwrap();
        let (name, _) = api_key.authorization().unwrap().unwrap();
        assert_eq!(name.as_str(), "api-key");
    }

    /// A credencial vai marcada como sensível: o `Debug` de `HeaderValue` imprime
    /// `Sensitive` em vez do conteúdo, então um log de debug do request não a vaza.
    #[test]
    fn credential_header_is_marked_sensitive_and_never_printed() {
        let provider = OpenAiCompatibleProvider::new(settings("https://example.com/v1")).unwrap();
        let (_, value) = provider.authorization().unwrap().unwrap();
        assert!(value.is_sensitive());
        assert!(!format!("{value:?}").contains("sk-chave-secreta"));
    }

    #[test]
    fn credential_with_control_characters_is_rejected_without_echoing_it() {
        let mut s = settings("https://example.com/v1");
        s.credential = Some("sk-quebra\nlinha".to_string());
        let provider = OpenAiCompatibleProvider::new(s).unwrap();
        let err = provider.authorization().unwrap_err();
        assert!(!err.to_string().contains("sk-quebra"));
        assert!(err.to_string().contains("authorization"));
    }

    /// Alguns gateways ecoam o cabeçalho recebido dentro do JSON de erro. Truncar limita o
    /// estrago e evita despejar página de proxy inteira no log.
    #[test]
    fn error_body_is_collapsed_and_truncated() {
        let long = "x".repeat(MAX_ERROR_BODY_CHARS * 2);
        let truncated = truncate_error_body(&long);
        assert_eq!(truncated.chars().count(), MAX_ERROR_BODY_CHARS + 1);
        assert!(truncated.ends_with('…'));
        assert_eq!(truncate_error_body("a\n\n  b\tc"), "a b c");
    }

    #[test]
    fn known_presets_point_at_their_documented_endpoints() {
        let cases = [
            (
                OpenAiCompatibleProvider::openai("k".into(), "m".into(), None).unwrap(),
                "https://api.openai.com",
                "openai",
            ),
            (
                OpenAiCompatibleProvider::deepseek("k".into(), "m".into(), None).unwrap(),
                "https://api.deepseek.com",
                "deepseek",
            ),
            (
                OpenAiCompatibleProvider::openrouter("k".into(), "m".into(), None).unwrap(),
                "https://openrouter.ai",
                "openrouter",
            ),
            (
                OpenAiCompatibleProvider::lm_studio("m".into(), None, None).unwrap(),
                "http://localhost:1234",
                "lm_studio",
            ),
        ];
        for (provider, endpoint, name) in cases {
            assert_eq!(provider.sanitized_endpoint(), endpoint);
            assert_eq!(provider.provider_name(), name);
        }
    }
}
