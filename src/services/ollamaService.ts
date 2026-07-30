/**
 * A direct client-side check against a local Ollama instance — not a Tauri command,
 * because it doesn't need to be one: `tauri.conf.json`'s CSP already allow-lists
 * `http://localhost:11434` in `connect-src` (the response-provider streaming path relies
 * on Ollama being reachable there too). This gives the AI-provider screen an honest
 * "Conectado"/not connected status instead of assuming success — and, when reachable,
 * a real list of installed models instead of a blind text field. Cloud providers
 * (OpenAI/Anthropic/DeepSeek) can't be checked this way: their domains aren't in the
 * CSP allow-list, so the browser itself blocks the request — which is why the
 * ai-provider screen only shows this kind of live status for Ollama.
 */
export interface OllamaProbeResult {
  reachable: boolean;
  models: string[];
}

export async function probeOllama(baseUrl: string | null): Promise<OllamaProbeResult> {
  const url = `${(baseUrl || "http://localhost:11434").replace(/\/$/, "")}/api/tags`;
  try {
    const controller = new AbortController();
    const timeout = window.setTimeout(() => controller.abort(), 2500);
    const response = await fetch(url, { signal: controller.signal });
    window.clearTimeout(timeout);
    if (!response.ok) return { reachable: false, models: [] };
    const data: unknown = await response.json();
    const models =
      typeof data === "object" && data && "models" in data && Array.isArray((data as { models: unknown }).models)
        ? ((data as { models: { name: string }[] }).models.map((m) => m.name).filter(Boolean))
        : [];
    return { reachable: true, models };
  } catch {
    return { reachable: false, models: [] };
  }
}
