import { useEffect, useId, useRef, useState } from "react";
import { Check, ChevronDown } from "lucide-react";
import { cx } from "../../utils/cx";

export interface SelectOption<T extends string> {
  value: T;
  label: string;
  /** Small trailing detail — "(padrão do Windows)", a device kind, etc. */
  detail?: string;
  disabled?: boolean;
}

interface SelectProps<T extends string> {
  label?: string;
  value: T | null;
  options: SelectOption<T>[];
  onChange: (value: T) => void;
  placeholder?: string;
  disabled?: boolean;
  className?: string;
}

/**
 * A "clean select" per docs/design-system.md — a listbox popover, not a native
 * `<select>` (which renders with the OS's own, inconsistent chrome and can't carry a
 * `detail` line). Used for language, device, and single-choice pickers. Falls back to
 * nothing fancy: a real button + a real listbox, both reachable and operable by keyboard
 * (Enter/Space opens, Arrow keys move, Enter selects, Escape closes and returns focus).
 */
export function Select<T extends string>({
  label,
  value,
  options,
  onChange,
  placeholder = "Selecionar",
  disabled,
  className,
}: SelectProps<T>) {
  const [open, setOpen] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const listId = useId();
  const selected = options.find((option) => option.value === value) ?? null;

  useEffect(() => {
    if (!open) return;
    const handlePointerDown = (event: PointerEvent) => {
      if (!containerRef.current?.contains(event.target as Node)) {
        setOpen(false);
      }
    };
    document.addEventListener("pointerdown", handlePointerDown);
    return () => document.removeEventListener("pointerdown", handlePointerDown);
  }, [open]);

  const closeAndFocusTrigger = () => {
    setOpen(false);
    triggerRef.current?.focus();
  };

  const moveSelection = (direction: 1 | -1) => {
    const enabled = options.filter((option) => !option.disabled);
    if (enabled.length === 0) return;
    const currentIndex = enabled.findIndex((option) => option.value === value);
    const nextIndex = (currentIndex + direction + enabled.length) % enabled.length;
    const next = enabled[nextIndex];
    if (next) onChange(next.value);
  };

  return (
    <div className={cx("flex flex-col gap-1.5", className)} ref={containerRef}>
      {label && <span className="text-xs font-medium text-neutral-400">{label}</span>}
      <div className="relative">
        <button
          ref={triggerRef}
          type="button"
          disabled={disabled}
          aria-haspopup="listbox"
          aria-expanded={open}
          aria-controls={listId}
          onClick={() => setOpen((v) => !v)}
          onKeyDown={(event) => {
            if (event.key === "ArrowDown") {
              event.preventDefault();
              if (open) moveSelection(1);
              else setOpen(true);
            } else if (event.key === "ArrowUp") {
              event.preventDefault();
              if (open) moveSelection(-1);
              else setOpen(true);
            } else if (event.key === "Escape" && open) {
              event.preventDefault();
              closeAndFocusTrigger();
            }
          }}
          className={cx(
            "flex w-full items-center justify-between gap-2 rounded-lg border border-white/12 bg-surface px-3 py-2.5 text-left text-sm text-neutral-100 transition-colors duration-150",
            "hover:border-white/20",
            open && "border-brand-400/70",
            "disabled:cursor-not-allowed disabled:opacity-50",
          )}
        >
          <span className={cx("truncate", !selected && "text-neutral-500")}>
            {selected ? selected.label : placeholder}
          </span>
          <ChevronDown
            className={cx("h-4 w-4 flex-shrink-0 text-neutral-500 transition-transform duration-150", open && "rotate-180")}
            aria-hidden="true"
          />
        </button>

        {open && (
          <ul
            id={listId}
            role="listbox"
            className="animate-rise-in absolute z-20 mt-1.5 w-full overflow-hidden rounded-lg border border-white/12 bg-surface-raised py-1 shadow-raised"
          >
            {options.map((option) => (
              <li key={option.value} role="presentation">
                <button
                  type="button"
                  role="option"
                  aria-selected={option.value === value}
                  disabled={option.disabled}
                  onClick={() => {
                    onChange(option.value);
                    closeAndFocusTrigger();
                  }}
                  className={cx(
                    "flex w-full items-center justify-between gap-3 px-3 py-2 text-left text-sm transition-colors duration-100",
                    "hover:bg-white/6",
                    option.value === value ? "text-neutral-100" : "text-neutral-300",
                    option.disabled && "cursor-not-allowed opacity-40",
                  )}
                >
                  <span className="flex flex-col">
                    <span>{option.label}</span>
                    {option.detail && <span className="text-xs text-neutral-500">{option.detail}</span>}
                  </span>
                  {option.value === value && <Check className="h-4 w-4 flex-shrink-0 text-brand-400" aria-hidden="true" />}
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}
