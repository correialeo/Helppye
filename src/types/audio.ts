export type AudioSourceKind = "microphone" | "system_output";

export interface AudioDevice {
  id: string;
  name: string;
  source: AudioSourceKind;
  is_default: boolean;
}

export type AudioCaptureEvent =
  | { type: "started"; device: AudioDevice }
  | {
      type: "frame";
      source: AudioSourceKind;
      samples: number[];
      sample_rate: number;
      channels: number;
      timestamp_ms: number;
    }
  | { type: "device_disconnected"; source: AudioSourceKind; device_id: string }
  | { type: "error"; source: AudioSourceKind; message: string }
  | { type: "stopped"; source: AudioSourceKind };

export type ResolutionSource = "persisted" | "windows_default" | "first_available_fallback";

export interface ResolvedDevice {
  device_id: string;
  device_name: string;
  is_windows_default: boolean;
  source: ResolutionSource;
}

export interface DeviceSelectionSnapshot {
  input: ResolvedDevice | null;
  output: ResolvedDevice | null;
}
