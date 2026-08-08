import { useCallback, useEffect, useState } from "react";
import {
  cancelModelDownload,
  getManagedModelsStatus,
  onModelDownloadEvent,
  selectManagedModel,
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
  const [models, setModels] = useState<ModelStatus[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [progress, setProgress] = useState<DownloadProgress | null>(null);

  const refresh = useCallback(() => {
    getManagedModelsStatus()
      .then((snapshot) => {
        setStatus(snapshot.active_model);
        setModels(snapshot.models);
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
        refresh();
      } else {
        // verifying/completed/cancelled/failed: the authoritative state lives in
        // `model_status_command`, not derived from the event stream.
        setProgress(null);
        refresh();
      }
    });
    return () => {
      unlistenPromise.then((unlisten) => unlisten());
    };
  }, [refresh]);

  const startDownload = useCallback(async (modelId?: string) => {
    try {
      await startModelDownload(modelId);
      // The command starts a background task. Refreshing here covers the fast path
      // where the file is already present (load/validation) while download events
      // continue to provide the authoritative state for a real transfer.
      refresh();
    } catch (e) {
      setError(String(e));
    }
  }, [refresh]);

  const selectModel = useCallback(async (modelId: string) => {
    try {
      await selectManagedModel(modelId);
      refresh();
    } catch (e) {
      setError(String(e));
    }
  }, [refresh]);

  const cancelDownload = useCallback(async () => {
    try {
      await cancelModelDownload();
    } catch {
      // Best-effort from the UI's perspective — the authoritative outcome arrives via
      // the cancelled/failed event, which triggers a refresh above.
    }
  }, []);

  return { status, models, error, progress, refresh, startDownload, selectModel, cancelDownload };
}
