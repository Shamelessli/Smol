/**
 * Extract a human-readable message from whatever Tauri throws on command
 * failure. Tauri rejects with a serialised AppError object
 * ({ kind: "Other", message: "…" }) rather than a JS Error instance, so we
 * probe for `.message` first, then fall back to the string form.
 */
export function extractErrorMessage(err: unknown): string {
  if (err instanceof Error) return err.message;
  if (
    err !== null &&
    typeof err === "object" &&
    "message" in err &&
    typeof (err as Record<string, unknown>).message === "string"
  ) {
    return (err as Record<string, unknown>).message as string;
  }
  return String(err);
}
