/** Root-mean-square level of a PCM frame, in dBFS (≤ 0, `-Infinity` for silence). */
export function rmsDbfs(samples: number[]): number {
  if (samples.length === 0) return -Infinity;
  const meanSquare = samples.reduce((sum, s) => sum + s * s, 0) / samples.length;
  const rms = Math.sqrt(meanSquare);
  return rms > 0 ? 20 * Math.log10(rms) : -Infinity;
}

/** Maps dBFS to a 0–100 meter fill. -60 dBFS (near-silence, the mic's practical noise
 * floor for normal speech input) reads as empty; 0 dBFS (digital full scale) as full. */
export function dbfsToPercent(levelDb: number): number {
  if (!Number.isFinite(levelDb)) return 0;
  return Math.min(100, Math.max(0, (levelDb + 60) * (100 / 60)));
}
