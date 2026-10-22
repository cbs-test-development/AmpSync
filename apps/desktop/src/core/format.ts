const KB = 1024;
const MB = KB * 1024;
const GB = MB * 1024;
const TB = GB * 1024;

export function num(n: number, digits = 0): string {
  return n.toLocaleString("en-US", { minimumFractionDigits: digits, maximumFractionDigits: digits });
}

/** A size split into a number and a unit, so the unit can be styled smaller. */
export function sizeParts(bytes: number): [string, string] {
  if (bytes >= TB) return [num(bytes / TB, 2), "TB"];
  if (bytes >= GB) return [num(bytes / GB, 2), "GB"];
  if (bytes >= MB) return [num(bytes / MB, bytes >= 100 * MB ? 0 : 1), "MB"];
  if (bytes >= KB) return [num(bytes / KB, 0), "KB"];
  return [num(bytes, 0), "B"];
}

export function size(bytes: number): string {
  const [v, u] = sizeParts(bytes);
  return `${v} ${u}`;
}

export function rate(bps: number): [string, string] {
  const [v, u] = sizeParts(bps);
  return [v, `${u}/s`];
}

export function duration(seconds: number): string {
  if (!isFinite(seconds) || seconds < 0) return "-";
  const s = Math.round(seconds);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  if (h > 0) return `${h}h ${String(m).padStart(2, "0")}m`;
  return `${m}m ${String(sec).padStart(2, "0")}s`;
}

export function ago(iso: string): string {
  const t = Date.parse(iso);
  if (isNaN(t)) return "";
  const s = Math.max(0, (Date.now() - t) / 1000);
  if (s < 90) return "just now";
  if (s < 3600) return `${Math.round(s / 60)} min ago`;
  if (s < 86400) return `${Math.round(s / 3600)} h ago`;
  return `${Math.round(s / 86400)} d ago`;
}

/** Join a folder and a name with the separator the folder already uses. */
export function joinPath(base: string, name: string): string {
  const sep = base.includes("\\") || /^[A-Za-z]:/.test(base) ? "\\" : "/";
  return base.endsWith("\\") || base.endsWith("/") ? base + name : base + sep + name;
}

/** Escape glob metacharacters so a literal path matches only itself. */
export function globEscape(p: string): string {
  return p.replace(/[*?[\]{}\\]/g, (c) => `[${c}]`);
}

export function initials(name: string): string {
  const parts = name.replace(/[-_.]+/g, " ").trim().split(/\s+/).filter(Boolean);
  return (parts.length > 1 ? parts[0][0] + parts[1][0] : (parts[0] ?? "?").slice(0, 2)).toUpperCase();
}
