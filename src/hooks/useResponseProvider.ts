import { useCallback, useEffect, useState } from "react";
import {
  deleteResponseProviderApiKey,
  getResponseProviderStatus,
  setResponseProviderApiKey,
  setResponseProviderConfig,
} from "../services/responseProviderService";
import type { ResponseProviderKind, ResponseProviderStatus } from "../types/responseProvider";

/** Response-suggestion provider status + mutations, shared by the ai-provider onboarding
 * screen and the settings screen (same data, same actions, two entry points). */
export function useResponseProvider() {
  const [status, setStatus] = useState<ResponseProviderStatus | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(() => {
    getResponseProviderStatus()
      .then((s) => {
        setStatus(s);
        setError(null);
      })
      .catch((e) => setError(String(e)));
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const saveConfig = useCallback(
    async (config: {
      provider: ResponseProviderKind;
      model: string;
      baseUrl: string | null;
      ollamaKeepAlive: string | null;
    }) => {
      await setResponseProviderConfig(config);
      refresh();
    },
    [refresh],
  );

  const saveApiKey = useCallback(
    async (provider: ResponseProviderKind, apiKey: string) => {
      await setResponseProviderApiKey(provider, apiKey);
      refresh();
    },
    [refresh],
  );

  const removeApiKey = useCallback(
    async (provider: ResponseProviderKind) => {
      await deleteResponseProviderApiKey(provider);
      refresh();
    },
    [refresh],
  );

  return { status, error, refresh, saveConfig, saveApiKey, removeApiKey };
}
