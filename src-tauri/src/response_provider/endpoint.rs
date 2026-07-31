//! Política de endpoint para provedores de resposta configuráveis pelo usuário.
//!
//! A partir do momento em que `base_url` e cabeçalhos passam a ser digitados na UI (LM
//! Studio, OpenRouter, proxy compatível com a API da OpenAI), o app deixa de falar só com
//! hosts que ele mesmo escolheu. O conteúdo enviado é a **conversa da reunião** — a coisa
//! mais sensível que este produto manipula. Este módulo é o único lugar onde uma URL de
//! provedor vira um destino aceito, e ele responde a três perguntas antes disso:
//!
//! 1. **É um esquema que sabemos falar?** Só `http` e `https`. `file://` leria disco,
//!    `ftp://`/`gopher://` e afins não são endpoints de chat, e qualquer esquema exótico é
//!    superfície de ataque sem contrapartida.
//! 2. **É local ou remoto?** A diferença importa para o usuário: `http://localhost:1234`
//!    (LM Studio) mantém a conversa na máquina; `https://openrouter.ai` a envia para
//!    terceiros. O app precisa dizer isso, então `EndpointClassification` é parte do
//!    retorno, não um detalhe interno.
//! 3. **O que pode ir para o log?** Nunca a URL inteira: `?api_key=...` em query string é
//!    um jeito comum (e ruim) de autenticar, e credencial embutida (`https://user:pass@`)
//!    é sintaxe válida de URL. `sanitized()` devolve apenas esquema, host e porta.
//!
//! **Sobre SSRF.** Um endpoint apontando para `169.254.169.254` (metadata de nuvem) ou para
//! um host interno é, por definição, o que o usuário pediu — este app não recebe URLs de
//! terceiros, ele recebe de quem está sentado na frente dele. O que este módulo faz **não**
//! é impedir o usuário de escolher um destino: é impedir que um destino escolhido por
//! engano passe despercebido. Por isso `EndpointClassification` distingue loopback, rede
//! privada/link-local e internet pública, e o chamador tem a informação para avisar. Ver
//! `docs/response-suggestion.md`, seção "Endpoints configuráveis".

use std::net::IpAddr;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use thiserror::Error;

/// Um provedor que demora mais que isso não serve para sugestão ao vivo: a fala já passou.
/// Vale para a requisição inteira, não só para a conexão, porque um stream que trava no
/// meio prende o slot de geração do turno.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// Ligar depois disso é inútil num endpoint de reunião ao vivo, e um host errado (typo no
/// IP) trava sem este teto até o timeout do SO.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Endpoints de chat não redirecionam em condições normais; um punhado de proxies faz
/// http→https. Redirect ilimitado, com header `Authorization` reenviado a cada salto,
/// entrega a credencial para o último host da cadeia — que não é o que o usuário digitou.
pub const MAX_REDIRECTS: usize = 2;

/// Cabeçalhos que o app monta sozinho a partir da configuração e da credencial. Deixar o
/// usuário sobrescrevê-los pela lista de cabeçalhos personalizados permitiria, por
/// exemplo, um `Authorization` digitado em texto puro anulando a chave do keychain — ou um
/// `Host` forjado apontando para outro serviço atrás do mesmo IP.
const RESERVED_HEADERS: [&str; 6] = [
    "authorization",
    "host",
    "content-length",
    "content-type",
    "connection",
    "transfer-encoding",
];

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EndpointError {
    #[error("endpoint inválido: {0}")]
    Malformed(String),
    #[error("esquema `{0}` não é suportado: use http ou https")]
    UnsupportedScheme(String),
    #[error("endpoint sem host")]
    MissingHost,
    #[error("credencial embutida na URL não é aceita: use o campo de chave de API")]
    CredentialsInUrl,
    #[error("cabeçalho `{0}` é reservado e não pode ser sobrescrito")]
    ReservedHeader(String),
    #[error("nome de cabeçalho inválido: `{0}`")]
    InvalidHeaderName(String),
    #[error("valor inválido para o cabeçalho `{0}`")]
    InvalidHeaderValue(String),
}

/// Para onde a conversa vai, do ponto de vista de quem está usando o app. É informação de
/// produto, não trivia de rede: é o que permite a UI dizer "isso sai da sua máquina".
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointClassification {
    /// `127.0.0.1`, `::1`, `localhost`. O conteúdo não sai da máquina.
    Loopback,
    /// RFC 1918, link-local, CGNAT, `.local`. Sai da máquina, mas não da rede.
    PrivateNetwork,
    /// Qualquer outro host. O conteúdo da reunião sai para um terceiro.
    PublicInternet,
}

impl EndpointClassification {
    /// Se o usuário precisa ser avisado de que a conversa deixa a máquina dele.
    pub fn leaves_machine(self) -> bool {
        !matches!(self, EndpointClassification::Loopback)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedEndpoint {
    url: reqwest::Url,
    classification: EndpointClassification,
}

impl ValidatedEndpoint {
    pub fn classification(&self) -> EndpointClassification {
        self.classification
    }

    /// A única forma segura de colocar um endpoint em log ou em mensagem de erro: esquema,
    /// host e porta, sem caminho, sem query e sem userinfo.
    pub fn sanitized(&self) -> String {
        let scheme = self.url.scheme();
        let host = self.url.host_str().unwrap_or("?");
        match self.url.port() {
            Some(port) => format!("{scheme}://{host}:{port}"),
            None => format!("{scheme}://{host}"),
        }
    }

    /// Concatena um caminho de API ao endpoint. Trabalha em cima da string base em vez de
    /// `Url::join` de propósito: `join` sobre `https://host/v1` com `"chat/completions"`
    /// descarta o `/v1`, o que faria toda instalação com prefixo de versão apontar para o
    /// lugar errado.
    pub fn endpoint_for(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.url.as_str().trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }
}

/// Valida e classifica uma `base_url` digitada pelo usuário.
pub fn validate_base_url(raw: &str) -> Result<ValidatedEndpoint, EndpointError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(EndpointError::Malformed("endpoint vazio".to_string()));
    }
    let url = reqwest::Url::parse(trimmed).map_err(|e| EndpointError::Malformed(e.to_string()))?;

    match url.scheme() {
        "http" | "https" => {}
        other => return Err(EndpointError::UnsupportedScheme(other.to_string())),
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(EndpointError::CredentialsInUrl);
    }
    let Some(host) = url.host_str() else {
        return Err(EndpointError::MissingHost);
    };

    Ok(ValidatedEndpoint {
        classification: classify_host(host),
        url,
    })
}

fn classify_host(host: &str) -> EndpointClassification {
    // `Url::host_str` mantém os colchetes de IPv6 literal; `IpAddr` não os aceita.
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bare.parse::<IpAddr>() {
        return classify_ip(ip);
    }
    let lower = host.to_ascii_lowercase();
    if lower == "localhost" || lower.ends_with(".localhost") {
        return EndpointClassification::Loopback;
    }
    // `.local` é mDNS: nome que só resolve dentro da rede em que a máquina está.
    if lower.ends_with(".local") || lower.ends_with(".internal") {
        return EndpointClassification::PrivateNetwork;
    }
    EndpointClassification::PublicInternet
}

fn classify_ip(ip: IpAddr) -> EndpointClassification {
    match ip {
        IpAddr::V4(v4) => {
            if v4.is_loopback() {
                EndpointClassification::Loopback
            } else if v4.is_private()
                || v4.is_link_local()
                // CGNAT (100.64.0.0/10): Tailscale e operadoras. Não é internet pública,
                // e `Ipv4Addr` não tem um predicado estável para essa faixa.
                || (v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]))
                || v4.is_unspecified()
            {
                EndpointClassification::PrivateNetwork
            } else {
                EndpointClassification::PublicInternet
            }
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback() {
                EndpointClassification::Loopback
            } else if v6.is_unspecified()
                // fc00::/7 (unique local) e fe80::/10 (link-local): sem predicado estável.
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
            {
                EndpointClassification::PrivateNetwork
            } else {
                EndpointClassification::PublicInternet
            }
        }
    }
}

/// Converte os cabeçalhos personalizados da configuração em `HeaderMap`, recusando os
/// reservados. Nomes e valores **nunca** são logados aqui: um cabeçalho personalizado é
/// exatamente onde uma credencial de proxy costuma ir.
pub fn build_custom_headers(pairs: &[(String, String)]) -> Result<HeaderMap, EndpointError> {
    let mut headers = HeaderMap::new();
    for (name, value) in pairs {
        let lower = name.trim().to_ascii_lowercase();
        if RESERVED_HEADERS.contains(&lower.as_str()) {
            return Err(EndpointError::ReservedHeader(lower));
        }
        let header_name = HeaderName::from_bytes(lower.as_bytes())
            .map_err(|_| EndpointError::InvalidHeaderName(lower.clone()))?;
        let mut header_value = HeaderValue::from_str(value.trim())
            .map_err(|_| EndpointError::InvalidHeaderValue(lower.clone()))?;
        // Marca o valor como sensível: o `Debug` de `HeaderMap` passa a imprimir `Sensitive`
        // em vez do conteúdo, então um `?headers` acidental em log não vaza a credencial.
        header_value.set_sensitive(true);
        headers.insert(header_name, header_value);
    }
    Ok(headers)
}

/// Traduz a falha de uma requisição para o erro tipado do domínio, sem deixar a URL
/// completa escapar. Existe aqui, e não em cada provider, por dois motivos: `reqwest::Error`
/// faz `Display` da URL inteira (query string com `?api_key=` inclusa), e "estourou o tempo"
/// precisa chegar como `Timeout`, não diluído em `Network` — os dois pedem ações diferentes
/// de quem está usando o app.
pub fn classify_request_error(
    sanitized_endpoint: &str,
    error: reqwest::Error,
) -> super::provider::ResponseProviderError {
    if error.is_timeout() {
        return super::provider::ResponseProviderError::Timeout(sanitized_endpoint.to_string());
    }
    super::provider::ResponseProviderError::Network(format!(
        "{sanitized_endpoint}: {}",
        error.without_url()
    ))
}

/// Cliente HTTP com a política deste módulo aplicada: timeout, teto de redirect e nenhum
/// cabeçalho implícito além dos que o provider monta.
pub fn build_client(default_headers: HeaderMap) -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
        .default_headers(default_headers)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::response_provider::provider::ResponseProviderError;

    #[test]
    fn accepts_http_and_https() {
        assert!(validate_base_url("http://localhost:1234/v1").is_ok());
        assert!(validate_base_url("https://api.openai.com/v1").is_ok());
    }

    #[test]
    fn rejects_non_http_schemes() {
        for raw in [
            "file:///etc/passwd",
            "ftp://example.com",
            "gopher://example.com",
            "ws://example.com",
            "data:text/plain,hello",
        ] {
            let err = validate_base_url(raw).unwrap_err();
            assert!(
                matches!(
                    err,
                    EndpointError::UnsupportedScheme(_) | EndpointError::MissingHost
                ),
                "{raw} deveria ser recusado, veio {err:?}"
            );
        }
    }

    #[test]
    fn rejects_credentials_embedded_in_the_url() {
        assert_eq!(
            validate_base_url("https://user:secret@example.com/v1").unwrap_err(),
            EndpointError::CredentialsInUrl
        );
        assert_eq!(
            validate_base_url("https://user@example.com/v1").unwrap_err(),
            EndpointError::CredentialsInUrl
        );
    }

    #[test]
    fn rejects_malformed_and_empty_input() {
        assert!(matches!(
            validate_base_url("").unwrap_err(),
            EndpointError::Malformed(_)
        ));
        assert!(matches!(
            validate_base_url("not a url").unwrap_err(),
            EndpointError::Malformed(_)
        ));
    }

    #[test]
    fn distinguishes_loopback_from_private_and_public() {
        let cases = [
            ("http://localhost:1234", EndpointClassification::Loopback),
            ("http://127.0.0.1:11434", EndpointClassification::Loopback),
            ("http://[::1]:1234", EndpointClassification::Loopback),
            (
                "http://192.168.0.10:1234",
                EndpointClassification::PrivateNetwork,
            ),
            (
                "http://10.1.2.3:1234",
                EndpointClassification::PrivateNetwork,
            ),
            (
                "http://169.254.169.254",
                EndpointClassification::PrivateNetwork,
            ),
            ("http://100.100.1.1", EndpointClassification::PrivateNetwork),
            (
                "http://servidor.local:1234",
                EndpointClassification::PrivateNetwork,
            ),
            (
                "https://openrouter.ai/api/v1",
                EndpointClassification::PublicInternet,
            ),
            ("https://8.8.8.8", EndpointClassification::PublicInternet),
        ];
        for (raw, expected) in cases {
            assert_eq!(
                validate_base_url(raw).unwrap().classification(),
                expected,
                "{raw}"
            );
        }
    }

    #[test]
    fn only_loopback_keeps_content_on_the_machine() {
        assert!(!EndpointClassification::Loopback.leaves_machine());
        assert!(EndpointClassification::PrivateNetwork.leaves_machine());
        assert!(EndpointClassification::PublicInternet.leaves_machine());
    }

    /// O que vai para o log não pode conter caminho, query nem fragmento — é lá que uma
    /// chave de API acaba quando o usuário cola a URL inteira que um serviço deu a ele.
    #[test]
    fn sanitized_form_drops_path_query_and_secrets() {
        let endpoint =
            validate_base_url("https://proxy.example.com:8443/v1?api_key=sk-abc123").unwrap();
        let sanitized = endpoint.sanitized();
        assert_eq!(sanitized, "https://proxy.example.com:8443");
        assert!(!sanitized.contains("sk-abc123"));
        assert!(!sanitized.contains("api_key"));
        assert!(!sanitized.contains("/v1"));
    }

    #[test]
    fn endpoint_path_preserves_the_version_prefix() {
        let endpoint = validate_base_url("https://api.openai.com/v1").unwrap();
        assert_eq!(
            endpoint.endpoint_for("chat/completions"),
            "https://api.openai.com/v1/chat/completions"
        );
        let with_slash = validate_base_url("http://localhost:1234/v1/").unwrap();
        assert_eq!(
            with_slash.endpoint_for("/chat/completions"),
            "http://localhost:1234/v1/chat/completions"
        );
    }

    #[test]
    fn custom_headers_cannot_override_reserved_ones() {
        for name in ["Authorization", "host", "Content-Type", "connection"] {
            let err = build_custom_headers(&[(name.to_string(), "x".to_string())]).unwrap_err();
            assert!(matches!(err, EndpointError::ReservedHeader(_)), "{name}");
        }
    }

    #[test]
    fn custom_headers_reject_invalid_names_and_values() {
        assert!(matches!(
            build_custom_headers(&[("bad header".to_string(), "v".to_string())]).unwrap_err(),
            EndpointError::InvalidHeaderName(_)
        ));
        assert!(matches!(
            build_custom_headers(&[("x-trace".to_string(), "linha\ninjetada".to_string())])
                .unwrap_err(),
            EndpointError::InvalidHeaderValue(_)
        ));
    }

    /// Um cabeçalho personalizado é onde credencial de proxy vive. Marcá-lo como sensível
    /// é o que impede que um `?headers` num log de debug imprima o valor.
    #[test]
    fn custom_header_values_are_marked_sensitive() {
        let headers =
            build_custom_headers(&[("x-proxy-token".to_string(), "segredo".to_string())]).unwrap();
        let value = headers.get("x-proxy-token").unwrap();
        assert!(value.is_sensitive());
        assert!(!format!("{headers:?}").contains("segredo"));
    }

    #[test]
    fn accepted_custom_headers_reach_the_map() {
        let headers = build_custom_headers(&[
            ("X-Title".to_string(), "Helppye".to_string()),
            (
                "HTTP-Referer".to_string(),
                "https://helppye.app".to_string(),
            ),
        ])
        .unwrap();
        assert_eq!(headers.len(), 2);
        assert!(headers.contains_key("x-title"));
        assert!(headers.contains_key("http-referer"));
    }

    /// Teto de tempo real, não só declarado: um servidor que aceita a conexão e nunca
    /// responde é o modo de falha típico de um LM Studio carregando modelo grande demais,
    /// e sem este teste nada garante que `REQUEST_TIMEOUT` chegou ao client.
    #[tokio::test]
    async fn a_server_that_never_answers_produces_a_typed_timeout_not_a_generic_network_error() {
        use std::time::Duration;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        // Aceita e some: o handshake TCP completa, então não é erro de conexão — é resposta
        // que nunca chega, que é exatamente o que `is_timeout()` precisa distinguir.
        tokio::spawn(async move {
            let _accepted = listener.accept().await;
            tokio::time::sleep(Duration::from_secs(30)).await;
        });

        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(150))
            .build()
            .unwrap();
        let error = client
            .post(format!("http://{address}/v1/chat/completions"))
            .send()
            .await
            .expect_err("servidor mudo não pode devolver sucesso");

        let sanitized = format!("http://{address}");
        match classify_request_error(&sanitized, error) {
            ResponseProviderError::Timeout(endpoint) => assert_eq!(endpoint, sanitized),
            other => panic!("esperava Timeout, veio {other:?}"),
        }
    }

    #[test]
    fn a_network_failure_keeps_the_sanitized_endpoint_and_never_the_full_url() {
        // Construir um `reqwest::Error` de rede sem rede real: um host inexistente resolve
        // e falha na conexão de forma determinística.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let error = runtime.block_on(async {
            reqwest::Client::new()
                .post("http://127.0.0.1:1/v1/chat/completions?api_key=sk-nao-deve-vazar")
                .send()
                .await
                .expect_err("porta 1 não aceita conexão")
        });

        let classified = classify_request_error("http://127.0.0.1:1", error);
        let message = classified.to_string();
        assert!(matches!(classified, ResponseProviderError::Network(_)));
        assert!(!message.contains("api_key"), "{message}");
        assert!(!message.contains("sk-nao-deve-vazar"), "{message}");
        assert!(!message.contains("chat/completions"), "{message}");
    }
}
