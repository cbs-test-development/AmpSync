/** Helpers for links that arrive by paste or drag, kept apart from the DOM so they can be tested. */

/** A drag carries a link when it offers a URL or plain text; a drag of files alone does not. */
export function carriesLink(types: readonly string[]): boolean {
  return types.includes("text/uri-list") || types.includes("text/plain");
}

/** True when the drag offers files; those are never accepted. */
export function carriesFiles(types: readonly string[]): boolean {
  return types.includes("Files");
}

/** The text to look for a link in: the uri-list when the source gave one, else the plain text. */
export function linkText(uriList: string, plain: string): string {
  const fromList = uriList
    .split(/\r?\n/)
    .map((l) => l.trim())
    .find((l) => l !== "" && !l.startsWith("#"));
  return fromList ?? plain.trim();
}

const NOT_TEXT_INPUTS = new Set(["checkbox", "radio", "button", "submit", "reset", "range", "color", "file", "image"]);

/** Whether a paste at `el` belongs to a text field (so the app must not take it over). */
export function isTextTarget(el: { tagName?: string; type?: string; isContentEditable?: boolean } | null): boolean {
  if (!el) return false;
  if (el.isContentEditable) return true;
  const tag = (el.tagName ?? "").toUpperCase();
  if (tag === "TEXTAREA" || tag === "SELECT") return true;
  return tag === "INPUT" && !NOT_TEXT_INPUTS.has((el.type ?? "text").toLowerCase());
}
