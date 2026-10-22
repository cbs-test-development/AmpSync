import { globEscape } from "./format";
import type { TreeNode } from "./types";

/**
 * Selection model for the folder picker.
 *
 * Every listed node has its own tick. A file belongs to the deepest listed folder that contains
 * it, so it follows that folder's tick. Root-level files follow their own tick.
 */
export type Ticks = Record<string, boolean>;

export function allTicked(nodes: TreeNode[]): Ticks {
  const t: Ticks = {};
  for (const n of nodes) t[n.path] = true;
  return t;
}

export function parentOf(path: string): string | null {
  const i = path.lastIndexOf("/");
  return i < 0 ? null : path.slice(0, i);
}

/** Tick or untick a node and everything below it. */
export function setTick(nodes: TreeNode[], ticks: Ticks, path: string, value: boolean): Ticks {
  const next = { ...ticks };
  const prefix = path + "/";
  for (const n of nodes) {
    if (n.path === path || n.path.startsWith(prefix)) next[n.path] = value;
  }
  return next;
}

/** True when some node below `path` differs from `path` itself (shows a partial tick). */
export function isPartial(nodes: TreeNode[], ticks: Ticks, path: string): boolean {
  const prefix = path + "/";
  const own = ticks[path];
  return nodes.some((n) => n.path.startsWith(prefix) && ticks[n.path] !== own);
}

export function totals(nodes: TreeNode[], ticks: Ticks): { files: number; bytes: number } {
  // A folder's counts include its listed sub-folders; subtract them to get what it owns itself.
  const childSum = new Map<string, { files: number; bytes: number }>();
  for (const n of nodes) {
    const p = parentOf(n.path);
    if (p === null || n.kind === "file") continue;
    const c = childSum.get(p) ?? { files: 0, bytes: 0 };
    c.files += n.files;
    c.bytes += n.bytes;
    childSum.set(p, c);
  }
  let files = 0, bytes = 0;
  for (const n of nodes) {
    if (!ticks[n.path]) continue;
    const c = childSum.get(n.path);
    files += n.files - (c?.files ?? 0);
    bytes += n.bytes - (c?.bytes ?? 0);
  }
  return { files, bytes };
}

/** Turn ticks into include/exclude globs for the engine. Both empty means "everything". */
export function toFilters(nodes: TreeNode[], ticks: Ticks): { include: string[]; exclude: string[] } {
  if (nodes.every((n) => ticks[n.path])) return { include: [], exclude: [] };
  const include: string[] = [];
  const exclude: string[] = [];
  for (const n of nodes) {
    const eff = ticks[n.path];
    const p = parentOf(n.path);
    const parentEff = p === null ? false : (ticks[p] ?? false);
    const pattern = n.kind === "file" ? globEscape(n.path) : globEscape(n.path) + "/**";
    if (eff && !parentEff) include.push(pattern);
    else if (!eff && parentEff) exclude.push(pattern);
  }
  return { include, exclude };
}
