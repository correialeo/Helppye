import { useCallback, useEffect, useState } from "react";
import { probeOllama, type OllamaProbeResult } from "../services/ollamaService";

export function useOllamaProbe(baseUrl: string | null, enabled: boolean) {
  const [result, setResult] = useState<OllamaProbeResult | null>(null);
  const [loading, setLoading] = useState(false);

  const refresh = useCallback(() => {
    if (!enabled) return;
    setLoading(true);
    probeOllama(baseUrl)
      .then(setResult)
      .finally(() => setLoading(false));
  }, [baseUrl, enabled]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  return { result, loading, refresh };
}
