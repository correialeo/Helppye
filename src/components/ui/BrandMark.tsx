import { AudioLines } from "lucide-react";
import { cx } from "../../utils/cx";

/**
 * Helppye's own mark: a solid brand-colored square with a simple audio-waveform glyph.
 * Original composition (generic icon + the app's single accent color) — not a stand-in
 * for, or a copy of, any other product's logo. Used wherever a small identity anchor is
 * useful (onboarding header, session header); never the app's primary visual content.
 */
export function BrandMark({ size = 28 }: { size?: number }) {
  return (
    <span
      className={cx(
      "inline-flex flex-shrink-0 items-center justify-center rounded-[8px] bg-white/[0.08] text-white shadow-[inset_0_0_0_1px_rgba(255,255,255,.08)]",
      )}
      style={{ width: size, height: size }}
      aria-hidden="true"
    >
      <AudioLines style={{ width: size * 0.58, height: size * 0.58 }} strokeWidth={2.25} />
    </span>
  );
}
