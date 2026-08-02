import { invoke } from "@tauri-apps/api/core";
import type {
  TranscriptionProviderDescriptor,
  TranscriptionProviderId,
  TranscriptionSettings,
} from "../types/transcriptionProvider";

export function getTranscriptionProviders(): Promise<TranscriptionProviderDescriptor[]> {
  return invoke("transcription_providers_command");
}

export function getTranscriptionSettings(): Promise<TranscriptionSettings> {
  return invoke("transcription_settings_command");
}

export function setTranscriptionSettings(settings: TranscriptionSettings): Promise<void> {
  return invoke("transcription_set_settings_command", { settings });
}

export function testTranscriptionConnection(settings: TranscriptionSettings): Promise<void> {
  return invoke("transcription_test_connection_command", { settings });
}

export function hasTranscriptionApiKey(provider: TranscriptionProviderId): Promise<boolean> {
  return invoke("transcription_has_api_key_command", { provider });
}

/** The key comes only from the password field and is sent directly to the OS keychain. */
export function storeTranscriptionApiKey(
  provider: TranscriptionProviderId,
  apiKey: string,
): Promise<void> {
  return invoke("transcription_store_api_key_command", { provider, apiKey });
}

export function deleteTranscriptionApiKey(provider: TranscriptionProviderId): Promise<void> {
  return invoke("transcription_delete_api_key_command", { provider });
}

export function validateActiveTranscriptionProvider(): Promise<void> {
  return invoke("transcription_validate_active_provider_command");
}
