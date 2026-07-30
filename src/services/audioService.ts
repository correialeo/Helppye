import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { AudioCaptureEvent, AudioDevice, AudioSourceKind, DeviceSelectionSnapshot } from "../types/audio";

export const AUDIO_CAPTURE_EVENT = "audio://capture-event";

export function onAudioCaptureEvent(handler: (event: AudioCaptureEvent) => void): Promise<UnlistenFn> {
  return listen<AudioCaptureEvent>(AUDIO_CAPTURE_EVENT, (event) => handler(event.payload));
}

export function resolveDeviceSelection(): Promise<DeviceSelectionSnapshot> {
  return invoke("resolve_device_selection_command");
}

export function listInputDevices(): Promise<AudioDevice[]> {
  return invoke("list_audio_devices_command");
}

export function listOutputDevices(): Promise<AudioDevice[]> {
  return invoke("list_system_audio_devices_command");
}

export function selectInputDevice(deviceId: string): Promise<void> {
  return invoke("select_input_device_command", { deviceId });
}

export function selectOutputDevice(deviceId: string): Promise<void> {
  return invoke("select_output_device_command", { deviceId });
}

export function startCapture(source: AudioSourceKind): Promise<void> {
  return invoke(source === "microphone" ? "start_microphone_capture_command" : "start_system_audio_capture_command");
}

export function stopCapture(source: AudioSourceKind): Promise<void> {
  return invoke(source === "microphone" ? "stop_microphone_capture_command" : "stop_system_audio_capture_command");
}
