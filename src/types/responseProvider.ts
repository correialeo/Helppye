export type ResponseProviderKind =
  | "ollama"
  | "lm_studio"
  | "open_ai"
  | "deep_seek"
  | "anthropic"
  | "open_router"
  | "custom_open_ai_compatible";

/** Como a credencial viaja para provedores compatíveis com a API da OpenAI. */
export type CredentialMode = "none" | "api_key" | "bearer_token";

/** Para onde a conversa vai, do ponto de vista de quem usa o app. */
export type EndpointClassification = "loopback" | "private_network" | "public_internet";

export interface EndpointStatus {
  /** Apenas `esquema://host:porta` — nunca caminho, query ou credencial. */
  sanitized: string;
  classification: EndpointClassification;
  leaves_machine: boolean;
}

/** O que o provedor sabe fazer. Vem da instância viva, não do catálogo: o mesmo LM Studio
 * é `local: true` em `localhost` e `local: false` apontando para outra máquina. */
export interface ResponseProviderCapabilities {
  local: boolean;
  streaming: boolean;
  requires_credentials: boolean;
  configurable_base_url: boolean;
  custom_headers: boolean;
}

export interface ResponseProviderStatus {
  provider: ResponseProviderKind;
  model: string;
  base_url: string | null;
  ollama_keep_alive: string | null;
  credential_mode: CredentialMode;
  custom_headers: [string, string][];
  requires_api_key: boolean;
  accepts_api_key: boolean;
  has_api_key: boolean;
  endpoint: EndpointStatus | null;
  capabilities: ResponseProviderCapabilities;
}
