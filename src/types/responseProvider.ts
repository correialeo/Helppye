export type ResponseProviderKind = "ollama" | "open_ai" | "deep_seek" | "anthropic";

export interface ResponseProviderStatus {
  provider: ResponseProviderKind;
  model: string;
  base_url: string | null;
  ollama_keep_alive: string | null;
  requires_api_key: boolean;
  has_api_key: boolean;
}
