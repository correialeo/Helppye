interface TranscriptPeekProps {
  text: string | null;
}

/** The other person's most recent line — secondary to the suggestion by design (smaller
 * type, muted color, capped height). This is context, not the point of the screen. */
export function TranscriptPeek({ text }: TranscriptPeekProps) {
  if (!text) return null;
  return (
    <div className="flex flex-col gap-0.5">
      <p className="text-xs font-medium text-neutral-500">Outra pessoa</p>
      <p className="line-clamp-2 text-sm leading-snug text-neutral-400">{text}</p>
    </div>
  );
}
