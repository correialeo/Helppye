import { type ReactNode, useEffect, useRef } from "react";
import { X } from "lucide-react";
import { IconButton } from "./IconButton";

interface DialogProps {
  open: boolean;
  onClose: () => void;
  title: string;
  children: ReactNode;
}

/**
 * Built on the native `<dialog>` element on purpose: `showModal()` gives us a real focus
 * trap, `Escape`-to-close, and — the requirement that's easy to get wrong by hand —
 * focus returning to whatever triggered the dialog when it closes, all without extra
 * code. Backdrop click closes too (a click landing on the `<dialog>` box itself, outside
 * the inner content wrapper, is the standard way to detect that).
 */
export function Dialog({ open, onClose, title, children }: DialogProps) {
  const dialogRef = useRef<HTMLDialogElement>(null);

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    if (open && !dialog.open) dialog.showModal();
    if (!open && dialog.open) dialog.close();
  }, [open]);

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    // "cancel" fires on Escape before the dialog closes itself; "close" covers every
    // other closing path (backdrop click below, the X button, dialog.close() above).
    const handleCancel = (event: Event) => {
      event.preventDefault();
      onClose();
    };
    const handleClose = () => onClose();
    dialog.addEventListener("cancel", handleCancel);
    dialog.addEventListener("close", handleClose);
    return () => {
      dialog.removeEventListener("cancel", handleCancel);
      dialog.removeEventListener("close", handleClose);
    };
  }, [onClose]);

  return (
    <dialog
      ref={dialogRef}
      onClick={(event) => {
        if (event.target === dialogRef.current) onClose();
      }}
      onCancel={(event) => event.preventDefault()}
      className="m-auto w-[min(28rem,calc(100vw-2.5rem))] rounded-xl2 border border-white/12 bg-surface-raised p-0 text-neutral-100 shadow-raised open:animate-rise-in [&::backdrop]:bg-black/60"
      aria-label={title}
    >
      <div className="flex items-center justify-between border-b border-white/10 px-4 py-3">
        <h2 className="text-sm font-semibold text-neutral-100">{title}</h2>
        <IconButton aria-label="Fechar" onClick={onClose}>
          <X className="h-4 w-4" />
        </IconButton>
      </div>
      <div className="p-4">{children}</div>
    </dialog>
  );
}
