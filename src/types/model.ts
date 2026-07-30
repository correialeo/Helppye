export type ModelInstallState =
  | { state: "not_installed" }
  | { state: "checking" }
  | { state: "downloading" }
  | { state: "cancelled" }
  | { state: "verifying" }
  | { state: "installing" }
  | { state: "ready" }
  | { state: "corrupted"; reason: string }
  | { state: "failed"; reason: string };

export interface ModelStatus {
  model_id: string;
  display_name: string;
  approximate_size_bytes: number;
  state: ModelInstallState;
  custom_model_path: string | null;
  language_support: "multilingual" | "english_only" | null;
}

export type ModelDownloadEvent =
  | { type: "started"; model_id: string; total_bytes: number }
  | {
      type: "progress";
      model_id: string;
      downloaded_bytes: number;
      total_bytes: number;
      bytes_per_second: number;
    }
  | { type: "verifying"; model_id: string }
  | { type: "completed"; model_id: string; path: string }
  | { type: "cancelled"; model_id: string }
  | { type: "failed"; model_id: string; error: string };
