/** Joins truthy class name fragments. Deliberately not `clsx`/`cva` — this is the one
 * predicate the whole UI kit needs, not a reason to add a dependency. */
export function cx(...classes: Array<string | false | null | undefined>): string {
  return classes.filter(Boolean).join(" ");
}
