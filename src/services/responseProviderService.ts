import { invoke } from "@tauri-apps/api/core";
import type { ResponseProviderKind, ResponseProviderStatus } from "../types/responseProvider";

export function getResponseProviderStatus(): Promise<ResponseProviderStatus> {
  return invoke("response_provider_status_command");
}

export function setResponseProviderConfig(config: {
  provider: ResponseProviderKind;
  model: string;
  baseUrl: string | null;
  ollamaKeepAlive: string | null;
  maximumAutomaticGenerationAgeMs?: number;
}): Promise<void> {
  return invoke("response_set_provider_config_command", config);
}

/** Never call with a value read back from storage — the caller must only ever hand this
 * a value the user just typed. Keys live in the OS keychain only; see
 * docs/design-system.md §Segurança. */
export function setResponseProviderApiKey(provider: ResponseProviderKind, apiKey: string): Promise<void> {
  return invoke("response_set_api_key_command", { provider, apiKey });
}

export function deleteResponseProviderApiKey(provider: ResponseProviderKind): Promise<void> {
  return invoke("response_delete_api_key_command", { provider });
}
