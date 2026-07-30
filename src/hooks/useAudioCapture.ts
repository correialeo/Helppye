import { useCallback, useEffect, useRef } from "react";
import {
  listInputDevices,
  listOutputDevices,
  onAudioCaptureEvent,
  resolveDeviceSelection,
  selectInputDevice,
  selectOutputDevice,
  startCapture as startCaptureCommand,
  stopCapture as stopCaptureCommand,
} from "../services/audioService";
import { useAudioCaptureStore } from "../stores/useAudioCaptureStore";
import { dbfsToPercent, rmsDbfs } from "../utils/audio";
import type { AudioSourceKind } from "../types/audio";

const SOURCES: AudioSourceKind[] = ["microphone", "system_output"];

/**
 * Mounted once, near the app root (see app/App.tsx) — owns the single global
 * `audio://capture-event` subscription and the initial device/selection load that feeds
 * `useAudioCaptureStore`. Renders nothing.
 */
export function AudioCaptureProvider() {
  const patch = useAudioCaptureStore((state) => state.patch);
  const decayTimers = useRef<Partial<Record<AudioSourceKind, number>>>({});

  const refreshDevices = useCallback(
    async (source: AudioSourceKind) => {
      try {
        const devices = await (source === "microphone" ? listInputDevices() : listOutputDevices());
        patch(source, { devices });
      } catch (e) {
        patch(source, { status: { kind: "error", message: String(e) } });
      }
    },
    [patch],
  );

  useEffect(() => {
    resolveDeviceSelection()
      .then((snapshot) => {
        patch("microphone", {
          selectedId: snapshot.input?.device_id ?? null,
          selectionSource: snapshot.input?.source ?? null,
        });
        patch("system_output", {
          selectedId: snapshot.output?.device_id ?? null,
          selectionSource: snapshot.output?.source ?? null,
        });
      })
      .catch(() => {});
    SOURCES.forEach((source) => void refreshDevices(source));
  }, [patch, refreshDevices]);

  useEffect(() => {
    const unlistenPromise = onAudioCaptureEvent((event) => {
      const source = event.type === "started" ? event.device.source : event.source;
      // While a device switch is in flight (`selectDevice` below), the backend's own
      // stop-then-restart on the new device fires started/stopped events that are just
      // implementation detail — suppress them so the UI doesn't flicker through
      // "parado" mid-switch. Read live from the store (not a ref captured at listener
      // registration time) since `selectDevice` lives in a different hook instance.
      if (useAudioCaptureStore.getState()[source].status.kind === "switching") return;

      if (event.type === "started") {
        patch(source, { status: { kind: "capturing" } });
      } else if (event.type === "frame") {
        patch(source, { levelDb: rmsDbfs(event.samples), status: { kind: "capturing" } });
        const timers = decayTimers.current;
        if (timers[source] !== undefined) window.clearTimeout(timers[source]);
        timers[source] = window.setTimeout(() => patch(source, { levelDb: -Infinity }), 300);
      } else if (event.type === "error") {
        patch(source, { status: { kind: "error", message: event.message } });
      } else if (event.type === "device_disconnected") {
        patch(source, { status: { kind: "disconnected" }, levelDb: -Infinity });
        void refreshDevices(source);
        resolveDeviceSelection()
          .then((snapshot) => {
            const resolved = source === "microphone" ? snapshot.input : snapshot.output;
            patch(source, {
              suggestedDevice: resolved,
              selectedId: resolved?.device_id ?? null,
              selectionSource: resolved?.source ?? null,
            });
          })
          .catch(() => {});
      } else if (event.type === "stopped") {
        useAudioCaptureStore.setState((state) => {
          const current = state[source].status.kind;
          if (current === "disconnected" || current === "error") return {};
          return { [source]: { ...state[source], status: { kind: "idle" } } } as never;
        });
      }
    });
    return () => {
      unlistenPromise.then((unlisten) => unlisten());
    };
  }, [patch, refreshDevices]);

  return null;
}

/** Per-source read/write handle for a single screen — thin wrapper over the shared
 * store + service calls, so screens never talk to `invoke` directly. */
export function useAudioCapture(source: AudioSourceKind) {
  const state = useAudioCaptureStore((s) => s[source]);
  const patch = useAudioCaptureStore((s) => s.patch);

  const start = useCallback(async () => {
    try {
      await startCaptureCommand(source);
      patch(source, { status: { kind: "capturing" } });
    } catch (e) {
      patch(source, { status: { kind: "error", message: String(e) } });
    }
  }, [source, patch]);

  const stop = useCallback(async () => {
    try {
      await stopCaptureCommand(source);
      patch(source, { status: { kind: "idle" } });
    } catch (e) {
      patch(source, { status: { kind: "error", message: String(e) } });
    }
  }, [source, patch]);

  const selectDevice = useCallback(
    async (deviceId: string) => {
      const wasCapturing = state.status.kind === "capturing";
      patch(source, { selectedId: deviceId, selectionSource: null, suggestedDevice: null });
      if (wasCapturing) patch(source, { status: { kind: "switching" } });
      try {
        await (source === "microphone" ? selectInputDevice(deviceId) : selectOutputDevice(deviceId));
        patch(source, { status: { kind: wasCapturing ? "capturing" : "idle" } });
      } catch (e) {
        patch(source, { status: { kind: "error", message: String(e) } });
      }
    },
    [source, state.status.kind, patch],
  );

  const useSuggestedDevice = useCallback(async () => {
    patch(source, { suggestedDevice: null });
    await start();
  }, [source, patch, start]);

  return {
    ...state,
    levelPercent: dbfsToPercent(state.levelDb),
    start,
    stop,
    selectDevice,
    useSuggestedDevice,
  };
}
