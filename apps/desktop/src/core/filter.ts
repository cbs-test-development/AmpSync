/**
 * Filters for the folder picker: pure functions over the flat arrays of a `PlanTree`, so the same
 * code runs in a Web Worker for big listings and in unit tests.
 *
 * A file matches when all of these hold:
 *
 * - the search text matches its name or the name of a folder above it;
 * - its extension is one of the ticked type chips (none ticked means any);
 * - its size is inside the size range;
 * - it was modified on or after the "since" time.
 *
 * Folders never match on their own: the tree shows the matching files and the folders that lead
 * to them.
 */
export interface FilterTree {
  names: string[];
  parents: ArrayLike<number>;
  dirs: ArrayLike<number>;
  sizes: ArrayLike<number>;
  /** Unix seconds; 0 for folders. */
  modified: ArrayLike<number>;
}

/** What the engine of the filter needs; every field is "off" when empty or 0. */
export interface FilterSpec {
  query: string;
  /** Lower-case extensions without the dot; "" stands for files without one. */
  exts: string[];
  /** Bytes, inclusive. */
  minSize: number;
  maxSize: number;
  /** Unix seconds, inclusive. */
  since: number;
}

export function emptySpec(): FilterSpec {
  return { query: "", exts: [], minSize: 0, maxSize: 0, since: 0 };
}

export function isActive(s: FilterSpec): boolean {
  return s.query.trim() !== "" || s.exts.length > 0 || s.minSize > 0 || s.maxSize > 0 || s.since > 0;
}

/** Extension of a file name, lower-case, without the dot. Dot-files and trailing dots have none. */
export function extOf(name: string): string {
  const dot = name.lastIndexOf(".");
  if (dot <= 0 || dot === name.length - 1) return "";
  return name.slice(dot + 1).toLowerCase();
}

/**
 * Matcher for the search box, on lower-case names. Without `*` or `?` it is a "contains" test;
 * with them the whole name has to fit the pattern, like a file manager's search (`*.pdf`,
 * `report 20??*`). Case-insensitive either way.
 */
export function nameMatcher(query: string): ((lowerName: string) => boolean) | null {
  const q = query.trim().toLowerCase();
  if (q === "") return null;
  if (!/[*?]/.test(q)) return (name) => name.includes(q);
  const body = q
    .replace(/\*+/g, "*")
    .split("")
    .map((c) => (c === "*" ? ".*" : c === "?" ? "." : c.replace(/[-.+^${}()|[\]\\/]/g, "\\$&")))
    .join("");
  const re = new RegExp(`^${body}$`, "s");
  return (name) => re.test(name);
}

/** Per-tree data the filter reuses on every keystroke. */
export interface FilterIndex {
  n: number;
  lower: string[];
  parents: Int32Array;
  dirs: Uint8Array;
  sizes: Float64Array;
  modified: Float64Array;
  /** Index into `extKeys` for each file; -1 for folders. */
  ext: Int32Array;
  extKeys: string[];
}

export function buildIndex(tree: FilterTree): FilterIndex {
  const n = tree.names.length;
  const lower = new Array<string>(n);
  const ext = new Int32Array(n).fill(-1);
  const extKeys: string[] = [];
  const ids = new Map<string, number>();
  for (let i = 0; i < n; i++) {
    lower[i] = tree.names[i].toLowerCase();
    if (!tree.dirs[i]) {
      const key = extOf(lower[i]);
      let id = ids.get(key);
      if (id === undefined) {
        id = extKeys.length;
        extKeys.push(key);
        ids.set(key, id);
      }
      ext[i] = id;
    }
  }
  return {
    n,
    lower,
    parents: Int32Array.from(tree.parents),
    dirs: Uint8Array.from(tree.dirs),
    sizes: Float64Array.from(tree.sizes),
    modified: Float64Array.from(tree.modified),
    ext,
    extKeys,
  };
}

export interface FilterResult {
  /** 1 for a file that matches. */
  match: Uint8Array;
  /** 1 for matching files and for every folder with one below it. */
  visible: Uint8Array;
  /** Matching files and their bytes below each node (a file counts itself). */
  files: Float64Array;
  bytes: Float64Array;
  /** Totals over the whole tree. */
  count: number;
  totalBytes: number;
}

export function runFilter(ix: FilterIndex, spec: FilterSpec): FilterResult {
  const { n, parents, dirs, sizes, modified, ext } = ix;
  const hit = nameMatcher(spec.query);
  let allowed: Uint8Array | null = null;
  if (spec.exts.length > 0) {
    allowed = new Uint8Array(ix.extKeys.length);
    const want = new Set(spec.exts);
    ix.extKeys.forEach((k, id) => {
      if (want.has(k)) allowed![id] = 1;
    });
  }
  const match = new Uint8Array(n);
  const files = new Float64Array(n);
  const bytes = new Float64Array(n);
  // 1 when the file's own name or a folder name above it matches the search text.
  const named = hit ? new Uint8Array(n) : null;
  for (let i = 0; i < n; i++) {
    if (named && hit) {
      const p = parents[i];
      if ((p >= 0 && named[p]) || hit(ix.lower[i])) named[i] = 1;
    }
    if (dirs[i]) continue;
    if (named && !named[i]) continue;
    if (allowed && !allowed[ext[i]]) continue;
    const size = sizes[i];
    if (size < spec.minSize || (spec.maxSize > 0 && size > spec.maxSize)) continue;
    if (spec.since > 0 && modified[i] < spec.since) continue;
    match[i] = 1;
    files[i] = 1;
    bytes[i] = size;
  }
  // Children come after parents, so one reverse pass rolls the matches up.
  const visible = new Uint8Array(n);
  let count = 0;
  let totalBytes = 0;
  for (let i = n - 1; i >= 0; i--) {
    if (files[i] === 0) continue;
    visible[i] = 1;
    const p = parents[i];
    if (p >= 0) {
      files[p] += files[i];
      bytes[p] += bytes[i];
    } else {
      count += files[i];
      totalBytes += bytes[i];
    }
  }
  return { match, visible, files, bytes, count, totalBytes };
}

export interface ExtStat {
  /** Lower-case extension without the dot; "" for files without one. */
  key: string;
  files: number;
  bytes: number;
}

/** Extensions present in the tree, biggest total size first. */
export function extensionStats(tree: Pick<FilterTree, "names" | "dirs" | "sizes">): ExtStat[] {
  const by = new Map<string, ExtStat>();
  for (let i = 0; i < tree.names.length; i++) {
    if (tree.dirs[i]) continue;
    const key = extOf(tree.names[i]);
    let s = by.get(key);
    if (!s) by.set(key, (s = { key, files: 0, bytes: 0 }));
    s.files += 1;
    s.bytes += tree.sizes[i];
  }
  return [...by.values()].sort((a, b) => b.bytes - a.bytes || b.files - a.files || a.key.localeCompare(b.key));
}

/** The chips shown directly (biggest by total size) and the rest for the "More" menu. */
export function splitChips(stats: ExtStat[], shown = 8): { top: ExtStat[]; more: ExtStat[] } {
  return { top: stats.slice(0, shown), more: stats.slice(shown) };
}

// ---------------------------------------------------------------- what the filter bar edits

export type SizeKind = "any" | "100mb" | "1gb" | "custom";
export type ModifiedKind = "any" | "7d" | "30d" | "since";

/** The filter bar's own state; `toSpec` turns it into what `runFilter` takes. */
export interface FilterState {
  query: string;
  exts: string[];
  size: SizeKind;
  /** Custom size range as typed (empty = no bound), in `unit`. */
  minCustom: string;
  maxCustom: string;
  unit: "MB" | "GB";
  modified: ModifiedKind;
  /** `YYYY-MM-DD` from the date input. */
  sinceDate: string;
}

export function emptyState(): FilterState {
  return {
    query: "",
    exts: [],
    size: "any",
    minCustom: "",
    maxCustom: "",
    unit: "MB",
    modified: "any",
    sinceDate: "",
  };
}

const MB = 1024 * 1024;
const GB = MB * 1024;
const DAY = 86400;

function typedSize(text: string, unit: "MB" | "GB"): number {
  const v = Number(text.replace(",", ".").trim());
  return Number.isFinite(v) && v > 0 ? Math.round(v * (unit === "GB" ? GB : MB)) : 0;
}

/** Local midnight of a `YYYY-MM-DD` date, in unix seconds; 0 when it is not a date. */
export function dayStart(date: string): number {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(date);
  if (!m) return 0;
  const t = new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3])).getTime();
  return Number.isFinite(t) ? Math.floor(t / 1000) : 0;
}

/** `now` is unix seconds. */
export function toSpec(s: FilterState, now = Math.floor(Date.now() / 1000)): FilterSpec {
  let minSize = 0;
  let maxSize = 0;
  if (s.size === "100mb") minSize = 100 * MB;
  else if (s.size === "1gb") minSize = GB;
  else if (s.size === "custom") {
    minSize = typedSize(s.minCustom, s.unit);
    maxSize = typedSize(s.maxCustom, s.unit);
  }
  let since = 0;
  if (s.modified === "7d") since = now - 7 * DAY;
  else if (s.modified === "30d") since = now - 30 * DAY;
  else if (s.modified === "since") since = dayStart(s.sinceDate);
  return { query: s.query, exts: s.exts, minSize, maxSize, since };
}
