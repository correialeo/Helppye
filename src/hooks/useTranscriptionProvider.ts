import { useCallback, useEffect, useState } from "react";
import {
  deleteTranscriptionApiKey,
  getTranscriptionProviders,
  getTranscriptionSettings,
  hasTranscriptionApiKey,
  setTranscriptionSettings,
  storeTranscriptionApiKey,
  testTranscriptionConnection,
} from "../services/transcriptionProviderService";
import type {
  TranscriptionConnectionState,
  TranscriptionProviderDescriptor,
  TranscriptionSettings,
} from "../types/transcriptionProvider";

export function useTranscriptionProvider() {
  const [descriptors, setDescriptors] = useState<TranscriptionProviderDescriptor[]>([]);
  const [settings, setSettings] = useState<TranscriptionSettings | null>(null);
  const [hasGeminiKey, setHasGeminiKey] = useState(false);
  const [connectionState, setConnectionState] =
    useState<TranscriptionConnectionState>("not_configured");
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const [nextDescriptors, nextSettings, hasKey] = await Promise.all([
        getTranscriptionProviders(),
        getTranscriptionSettings(),
        hasTranscriptionApiKey("google_gemini"),
      ]);
      setDescriptors(nextDescriptors);
      setSettings(nextSettings);
      setHasGeminiKey(hasKey);
      setConnectionState(
        nextSettings.provider === "google_gemini" && hasKey ? "connected" : "not_configured",
      );
      setError(null);
    } catch (cause) {
      setError(String(cause));
      setConnectionState("error");
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const activateLocal = useCallback(async () => {
    if (!settings) return false;
    const next: TranscriptionSettings = {
      ...settings,
      provider: "whisper_local",
      language: { mode: "fixed", tag: "pt" },
      model: null,
    };
    try {
      await setTranscriptionSettings(next);
      setSettings(next);
      setConnectionState("not_configured");
      setError(null);
      return true;
    } catch (cause) {
      setError(String(cause));
      return false;
    }
  }, [settings]);

  const connectGemini = useCallback(
    async (apiKey: string, model: string) => {
      if (!settings) return;
      setConnectionState("connecting");
      setError(null);
      try {
        if (apiKey.trim()) {
          await storeTranscriptionApiKey("google_gemini", apiKey);
          setHasGeminiKey(true);
        }
        const next: TranscriptionSettings = {
          ...settings,
          provider: "google_gemini",
          language: { mode: "automatic" },
          model: null,
          providers: {
            ...settings.providers,
            google_gemini: {
              ...settings.providers.google_gemini,
              model: model.trim(),
            },
          },
        };
        await testTranscriptionConnection(next);
        await setTranscriptionSettings(next);
        setSettings(next);
        setConnectionState("connected");
      } catch (cause) {
        setError(String(cause));
        setConnectionState("error");
      }
    },
    [settings],
  );

  const removeGeminiKey = useCallback(async () => {
    await deleteTranscriptionApiKey("google_gemini");
    setHasGeminiKey(false);
    setConnectionState("not_configured");
    setError(null);
  }, []);

  return {
    descriptors,
    settings,
    hasGeminiKey,
    connectionState,
    error,
    activateLocal,
    connectGemini,
    removeGeminiKey,
  };
}
