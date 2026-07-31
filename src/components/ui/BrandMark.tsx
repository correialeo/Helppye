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
      "inline-flex flex-shrink-0 items-center justify-center rounded-[6px] bg-brand-600 text-white shadow-[0_0_16px_rgba(37,99,235,.32)]",
      )}
      style={{ width: size, height: size }}
      aria-hidden="true"
    >
      <AudioLines style={{ width: size * 0.58, height: size * 0.58 }} strokeWidth={2.25} />
    </span>
  );
}
