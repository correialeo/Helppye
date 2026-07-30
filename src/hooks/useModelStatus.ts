import { useCallback, useEffect, useState } from "react";
import {
  cancelModelDownload,
  getModelStatus,
  onModelDownloadEvent,
  startModelDownload,
} from "../services/modelService";
import type { ModelStatus } from "../types/model";

interface DownloadProgress {
  downloaded: number;
  total: number;
  bytesPerSecond: number;
}

/** Local transcription model lifecycle — status polling + live download progress. Used
 * by the audio-setup screen (the model must be ready before the guided audio test makes
 * sense) and by developer tools (raw status). Never starts a download on its own: see
 * `startDownload` below and CLAUDE.md's "never a silent download" rule. */
export function useModelStatus() {
  const [status, setStatus] = useState<ModelStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [progress, setProgress] = useState<DownloadProgress | null>(null);

  const refresh = useCallback(() => {
    getModelStatus()
      .then((s) => {
        setStatus(s);
        setError(null);
      })
      .catch((e) => setError(String(e)));
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  useEffect(() => {
    const unlistenPromise = onModelDownloadEvent((event) => {
      if (event.type === "progress") {
        setProgress({
          downloaded: event.downloaded_bytes,
          total: event.total_bytes,
          bytesPerSecond: event.bytes_per_second,
        });
      } else if (event.type === "started") {
        setProgress({ downloaded: 0, total: event.total_bytes, bytesPerSecond: 0 });
      } else {
        // verifying/completed/cancelled/failed: the authoritative state lives in
        // `model_status_command`, not derived from the event stream.
        refresh();
      }
    });
    return () => {
      unlistenPromise.then((unlisten) => unlisten());
    };
  }, [refresh]);

  const startDownload = useCallback(async () => {
    try {
      await startModelDownload();
      setStatus((s) => (s ? { ...s, state: { state: "downloading" } } : s));
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const cancelDownload = useCallback(async () => {
    try {
      await cancelModelDownload();
    } catch {
      // Best-effort from the UI's perspective — the authoritative outcome arrives via
      // the cancelled/failed event, which triggers a refresh above.
    }
  }, []);

  return { status, error, progress, refresh, startDownload, cancelDownload };
}
