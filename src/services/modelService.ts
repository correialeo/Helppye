import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { ModelDownloadEvent, ModelStatus } from "../types/model";

export const MODEL_DOWNLOAD_EVENT = "model-download://event";

export function getModelStatus(): Promise<ModelStatus> {
  return invoke("model_status_command");
}

export function startModelDownload(modelId?: string): Promise<void> {
  return invoke("start_model_download_command", { modelId });
}

export function cancelModelDownload(): Promise<void> {
  return invoke("cancel_model_download_command");
}

export function selectCustomModel(modelPath: string, modelName: string): Promise<void> {
  return invoke("select_custom_model_command", { modelPath, modelName });
}

export function onModelDownloadEvent(handler: (event: ModelDownloadEvent) => void): Promise<UnlistenFn> {
  return listen<ModelDownloadEvent>(MODEL_DOWNLOAD_EVENT, (event) => handler(event.payload));
}
