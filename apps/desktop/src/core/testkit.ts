// Test helper: builds a `PlanTree` from file paths the way the backend lays it out (folders before
// files, each group sorted case-insensitively, parents before children). Not part of the app.
import type { PlanTree } from "./types";

export interface TestFile {
  path: string;
  size?: number;
  modified?: number;
}

export function treeOf(files: TestFile[]): PlanTree {
  interface N {
    name: string;
    dir: boolean;
    size: number;
    modified: number;
    kids: Map<string, N>;
  }
  const root: N = { name: "", dir: true, size: 0, modified: 0, kids: new Map() };
  for (const f of files) {
    const parts = f.path.split("/");
    let at = root;
    parts.forEach((part, i) => {
      const last = i === parts.length - 1;
      const key = part.toLowerCase() + (last ? "\0f" : "\0d");
      let n = at.kids.get(key);
      if (!n) {
        n = { name: part, dir: !last, size: last ? (f.size ?? 1) : 0, modified: last ? (f.modified ?? 0) : 0, kids: new Map() };
        at.kids.set(key, n);
      }
      at = n;
    });
  }
  const out: PlanTree = {
    total_files: files.length,
    total_bytes: files.reduce((a, f) => a + (f.size ?? 1), 0),
    listed_ms: 0,
    names: [],
    parents: [],
    dirs: [],
    sizes: [],
    modified: [],
  };
  const walk = (n: N, parent: number) => {
    const lower = (k: N) => k.name.toLowerCase();
    const kids = [...n.kids.values()].sort((a, b) => Number(b.dir) - Number(a.dir) || (lower(a) < lower(b) ? -1 : lower(a) > lower(b) ? 1 : 0));
    for (const k of kids) {
      const at = out.names.length;
      out.names.push(k.name);
      out.parents.push(parent);
      out.dirs.push(k.dir ? 1 : 0);
      out.sizes.push(k.size);
      out.modified.push(k.modified);
      walk(k, at);
    }
  };
  walk(root, -1);
  return out;
}
