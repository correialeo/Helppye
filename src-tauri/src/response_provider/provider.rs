//! Abstração de provedor de sugestão de resposta. Cada implementação recebe um
//! `ResponseRequest` já montado (system prompt + histórico limitado) e devolve um stream
//! de deltas de texto — nunca a resposta inteira de uma vez, para manter a latência de
//! ponta a ponta baixa.

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Identidade estável de um backend de geração. Existe para que "qual provedor" seja um
/// valor tipado — comparável, serializável, exaustivo num `match` — em vez de uma string
/// solta. `ResponseProviderKind` (a configuração persistida) converte para este tipo; os
/// dois são separados porque a configuração é um formato de arquivo com compatibilidade a
/// manter, e este é a identidade em memória.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseProviderId {
    Ollama,
    LmStudio,
    OpenAi,
    DeepSeek,
    Anthropic,
    OpenRouter,
    CustomOpenAiCompatible,
    /// Assinatura ChatGPT Plus/Pro via autenticação de conta. **Não implementado** — ver
    /// `docs/adr/chatgpt-codex-subscription-auth.md`. Existe como variante para que a
    /// ausência seja explícita e listável na UI, não para que algo a construa.
    ChatGptCodexAccount,
    /// Estado de configuração inválida (sem credencial, endpoint recusado). Não é um
    /// backend: é o que o motor usa para falhar com uma mensagem em vez de entregar um
    /// provider que finge funcionar.
    Misconfigured,
}

impl ResponseProviderId {
    pub fn as_str(self) -> &'static str {
        match self {
            ResponseProviderId::Ollama => "ollama",
            ResponseProviderId::LmStudio => "lm_studio",
            ResponseProviderId::OpenAi => "openai",
            ResponseProviderId::DeepSeek => "deepseek",
            ResponseProviderId::Anthropic => "anthropic",
            ResponseProviderId::OpenRouter => "openrouter",
            ResponseProviderId::CustomOpenAiCompatible => "custom_openai_compatible",
            ResponseProviderId::ChatGptCodexAccount => "chatgpt_codex_account",
            ResponseProviderId::Misconfigured => "misconfigured",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            ResponseProviderId::Ollama => "Ollama (local)",
            ResponseProviderId::LmStudio => "LM Studio (local)",
            ResponseProviderId::OpenAi => "OpenAI",
            ResponseProviderId::DeepSeek => "DeepSeek",
            ResponseProviderId::Anthropic => "Anthropic",
            ResponseProviderId::OpenRouter => "OpenRouter",
            ResponseProviderId::CustomOpenAiCompatible => "Endpoint compatível com a OpenAI",
            ResponseProviderId::ChatGptCodexAccount => "Conta ChatGPT Plus/Pro",
            ResponseProviderId::Misconfigured => "Configuração inválida",
        }
    }
}

/// O que um backend declara saber fazer. Declarado, não inferido: a UI precisa dizer
/// "isso não sai da sua máquina" ou "isso exige uma chave" **antes** de o usuário falar
/// numa reunião, e não depois de a primeira geração falhar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ResponseProviderCapabilities {
    /// A conversa não sai da máquina do usuário.
    pub local: bool,
    /// Entrega deltas de texto em streaming. Hoje todos os implementados entregam; a flag
    /// existe porque um backend sem streaming exigiria outra experiência de UI (a sugestão
    /// aparecendo de uma vez, com a latência inteira visível).
    pub streaming: bool,
    /// Precisa de credencial no keychain para funcionar.
    pub requires_credentials: bool,
    /// Aceita `base_url` definida pelo usuário.
    pub configurable_base_url: bool,
    /// Aceita cabeçalhos HTTP adicionais definidos pelo usuário.
    pub custom_headers: bool,
}

impl ResponseProviderCapabilities {
    /// O mínimo declarável. Usado para backends previstos mas não implementados: afirmar
    /// capacidade de algo que não existe nesta build é o mesmo erro que um provider que
    /// finge funcionar.
    pub const fn none() -> Self {
        ResponseProviderCapabilities {
            local: false,
            streaming: false,
            requires_credentials: false,
            configurable_base_url: false,
            custom_headers: false,
        }
    }
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// Baixa de propósito (ver `context::TEMPERATURE`) — sugestão de resposta em reunião
    /// ao vivo se beneficia de previsibilidade, não de criatividade.
    pub temperature: f32,
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
    /// Tempo esgotado antes de o provedor responder. Separado de `Network` porque a ação
    /// que resolve cada um é diferente: um endpoint local que estoura o tempo quase sempre
    /// é modelo grande demais para a máquina, não cabo solto — e a UI precisa poder dizer
    /// isso. O conteúdo é sempre o endpoint **sanitizado**, nunca a URL completa.
    #[error("tempo esgotado esperando {0}")]
    Timeout(String),
    #[error("resposta inválida do provedor: {0}")]
    InvalidResponse(String),
    #[error("credencial ausente ou inválida: {0}")]
    Credential(String),
    #[error("provedor retornou erro: {0}")]
    Provider(String),
}

pub type ResponseStream = BoxStream<'static, Result<ResponseChunk, ResponseProviderError>>;

/// Metadados da requisição HTTP que produziu o stream, capturados para diagnóstico
/// (ver `docs/response-suggestion.md`). Providers sem uma chamada HTTP real (ex.:
/// `MisconfiguredProvider`) nunca chegam a construir isso, já que falham antes.
#[derive(Debug, Clone, Copy)]
pub struct ResponseStreamMeta {
    pub http_status: u16,
}

/// Contrato de um backend de geração.
///
/// A assinatura é `stream_reply(request) -> stream`, e não
/// `generate(request, sink, cancellation)`. A diferença é deliberada: com um `sink` e um
/// token de cancelamento nas mãos, cada provider passaria a ter como (e ser tentado a)
/// emitir eventos, decidir estado terminal, aplicar `[SKIP]` e medir latência — que é
/// exatamente a duplicação que se quer evitar. Devolvendo só o stream, o provider fica
/// responsável por uma coisa: falar o protocolo do serviço dele. Cancelamento, supressão
/// de eco, detecção de `[SKIP]`, validação de sessão, estado terminal e diagnósticos
/// moram no `ResponseEngine`, uma vez só, para todos os backends — e um provider novo não
/// tem como esquecer nenhum deles.
#[async_trait]
pub trait ResponseProvider: Send + Sync {
    fn id(&self) -> ResponseProviderId;

    fn capabilities(&self) -> ResponseProviderCapabilities;

    /// Nome curto usado em log e em diagnóstico. Deriva do `id` por padrão para que não
    /// exista um segundo lugar onde o nome de um provider possa divergir.
    fn provider_name(&self) -> &'static str {
        self.id().as_str()
    }

    async fn stream_reply(
        &self,
        request: ResponseRequest,
    ) -> Result<(ResponseStream, ResponseStreamMeta), ResponseProviderError>;
}
