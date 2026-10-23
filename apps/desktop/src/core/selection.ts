import { globEscape } from "./format";
import type { PlanTree } from "./types";

/**
 * Selection model for the folder picker.
 *
 * The backend sends the listing as flat arrays in display order: parents come before their
 * children and every subtree is one contiguous range `[i, end[i])`. That makes everything here a
 * plain loop over typed arrays, which stays fast for ~100k files:
 *
 * - rollups (`files`, `bytes`) are computed once, in one reverse pass;
 * - a file is selected or not; a folder is derived (all / none / some of its files);
 * - toggling a node rewrites its range and adds one delta to each ancestor.
 *
 * Folders without any file below them (empty folders) carry their own flag so they can still be
 * ticked; they never produce a pattern because the engine only downloads files.
 */
export type CheckState = 0 | 1 | 2; // unchecked, checked, partly checked

export interface Totals {
  files: number;
  bytes: number;
}

export class TreeModel {
  readonly n: number;
  readonly names: string[];
  readonly parent: Int32Array;
  readonly isDir: Uint8Array;
  /** Exclusive end of the subtree that starts at each node. */
  readonly end: Int32Array;
  readonly depth: Uint16Array;
  /** Files and bytes below each node (a file counts itself). */
  readonly files: Float64Array;
  readonly bytes: Float64Array;
  /** The selected part of the same. */
  readonly selFiles: Float64Array;
  readonly selBytes: Float64Array;
  /** Own tick of an empty folder. */
  private readonly flag: Uint8Array;
  readonly allFiles: number;
  readonly allBytes: number;
  private totalFiles: number;
  private totalBytes: number;

  constructor(tree: Pick<PlanTree, "names" | "parents" | "dirs" | "sizes">) {
    const n = tree.names.length;
    this.n = n;
    this.names = tree.names;
    this.parent = Int32Array.from(tree.parents);
    this.isDir = Uint8Array.from(tree.dirs);
    this.end = new Int32Array(n);
    this.depth = new Uint16Array(n);
    this.files = new Float64Array(n);
    this.bytes = new Float64Array(n);
    this.flag = new Uint8Array(n).fill(1);
    for (let i = 0; i < n; i++) {
      const p = this.parent[i];
      this.depth[i] = p < 0 ? 0 : Math.min(this.depth[p] + 1, 65535);
      this.end[i] = i + 1;
      if (!this.isDir[i]) {
        this.files[i] = 1;
        this.bytes[i] = tree.sizes[i];
      }
    }
    // Children come after parents, so one reverse pass rolls everything up.
    for (let i = n - 1; i >= 0; i--) {
      const p = this.parent[i];
      if (p >= 0) {
        this.files[p] += this.files[i];
        this.bytes[p] += this.bytes[i];
        if (this.end[i] > this.end[p]) this.end[p] = this.end[i];
      }
    }
    this.selFiles = Float64Array.from(this.files);
    this.selBytes = Float64Array.from(this.bytes);
    let tf = 0;
    let tb = 0;
    for (let i = 0; i < n; i++) {
      if (this.parent[i] < 0) {
        tf += this.files[i];
        tb += this.bytes[i];
      }
    }
    this.allFiles = tf;
    this.allBytes = tb;
    this.totalFiles = tf;
    this.totalBytes = tb;
  }

  /** What is selected right now. */
  get selected(): Totals {
    return { files: this.totalFiles, bytes: this.totalBytes };
  }

  hasChildren(i: number): boolean {
    return this.isDir[i] === 1 && this.end[i] > i + 1;
  }

  state(i: number): CheckState {
    if (this.files[i] === 0) return this.flag[i] ? 1 : 0;
    const s = this.selFiles[i];
    if (s === 0) return 0;
    return s === this.files[i] ? 1 : 2;
  }

  /** Slash-joined path of a node, for patterns and tooltips. */
  path(i: number): string {
    const parts: string[] = [];
    for (let k = i; k >= 0; k = this.parent[k]) parts.push(this.names[k]);
    return parts.reverse().join("/");
  }

  /** Tick or untick a node and everything below it. */
  set(i: number, value: boolean) {
    const before = this.selFiles[i];
    const beforeBytes = this.selBytes[i];
    const end = this.end[i];
    for (let j = i; j < end; j++) {
      this.selFiles[j] = value ? this.files[j] : 0;
      this.selBytes[j] = value ? this.bytes[j] : 0;
      if (this.files[j] === 0) this.flag[j] = value ? 1 : 0;
    }
    const dF = this.selFiles[i] - before;
    const dB = this.selBytes[i] - beforeBytes;
    for (let p = this.parent[i]; p >= 0; p = this.parent[p]) {
      this.selFiles[p] += dF;
      this.selBytes[p] += dB;
    }
    this.totalFiles += dF;
    this.totalBytes += dB;
  }

  /** What a click on the checkbox does: a fully ticked node is unticked, anything else is ticked. */
  toggle(i: number) {
    this.set(i, this.state(i) !== 1);
  }

  /**
   * Tick or untick only the files flagged in `match` (a filter result) inside `[from, to)`;
   * everything else keeps its state. A folder row in a filtered tree uses its own range, the
   * "Select matching" / "Clear matching" buttons use the whole tree.
   */
  setMatching(from: number, to: number, value: boolean, match: Uint8Array) {
    for (let j = from; j < to; j++) {
      if (this.isDir[j] || !match[j] || (this.selFiles[j] === 1) === value) continue;
      const d = value ? 1 : -1;
      const db = d * this.bytes[j];
      this.selFiles[j] = value ? 1 : 0;
      this.selBytes[j] = value ? this.bytes[j] : 0;
      for (let p = this.parent[j]; p >= 0; p = this.parent[p]) {
        this.selFiles[p] += d;
        this.selBytes[p] += db;
      }
      this.totalFiles += d;
      this.totalBytes += db;
    }
  }

  /** Slash-joined paths of every selected file, in display order. */
  selectedFiles(): string[] {
    const out: string[] = [];
    // Folder paths are built once and reused by everything below them.
    const prefix = new Array<string>(this.n);
    for (let i = 0; i < this.n; i++) {
      const p = this.parent[i];
      const path = p < 0 ? this.names[i] : `${prefix[p]}/${this.names[i]}`;
      if (this.isDir[i]) prefix[i] = path;
      else if (this.selFiles[i] === 1) out.push(path);
    }
    return out;
  }

  /**
   * Include/exclude globs for the engine (`JobSpec.include` / `exclude`), both empty meaning
   * "everything".
   *
   * The engine downloads a file when it matches `include` (or include is empty) and does not
   * match `exclude`; exclude always wins, so a ticked folder cannot be re-included inside an
   * unticked one. Two forms express any selection, and the shorter list is used:
   *
   * - exclude form: one `folder/**` (or file) pattern per outermost unticked subtree;
   * - include form: one pattern per outermost fully ticked subtree.
   *
   * On a tie the exclude form wins, so files added to a ticked folder on SharePoint later are
   * still picked up by a rerun.
   */
  filters(): { include: string[]; exclude: string[] } {
    if (this.totalFiles === this.allFiles) return { include: [], exclude: [] };
    const inc: number[] = [];
    const exc: number[] = [];
    let i = 0;
    while (i < this.n) {
      if (this.files[i] === 0) {
        i = this.end[i];
        continue;
      }
      const s = this.selFiles[i];
      if (s === this.files[i]) {
        inc.push(i);
        i = this.end[i];
      } else if (s === 0) {
        exc.push(i);
        i = this.end[i];
      } else {
        i++; // partly selected: look inside
      }
    }
    const pattern = (k: number) => globEscape(this.path(k)) + (this.isDir[k] ? "/**" : "");
    return inc.length < exc.length
      ? { include: inc.map(pattern), exclude: [] }
      : { include: [], exclude: exc.map(pattern) };
  }
}

/** Indices of the rows to show: every root, plus the children of each expanded folder. */
export function visibleRows(model: TreeModel, expanded: Uint8Array): Int32Array {
  const out = new Int32Array(model.n);
  let count = 0;
  let i = 0;
  while (i < model.n) {
    out[count++] = i;
    i = model.isDir[i] && !expanded[i] ? model.end[i] : i + 1;
  }
  return out.subarray(0, count);
}

/** Per node: how many of the files flagged in `match` are selected (a file counts itself). */
export function selectedAmong(model: TreeModel, match: Uint8Array): Float64Array {
  const out = new Float64Array(model.n);
  for (let i = model.n - 1; i >= 0; i--) {
    if (!model.isDir[i] && match[i] && model.selFiles[i] === 1) out[i] = 1;
    const p = model.parent[i];
    if (p >= 0) out[p] += out[i];
  }
  return out;
}

/** The most globs sent as `include` / `exclude` before the explicit file list is used instead. */
export const MAX_GLOBS = 50;

/** The selection as the engine's job description (`JobSpec` fields, D20). */
export interface SelectionSpec {
  include: string[];
  exclude: string[];
  /** Exactly these files; set only when the globs would be too many, and then both lists are empty. */
  files: string[] | null;
}

/**
 * Turn the selection into a job: the compact globs when there are at most `maxGlobs`, else the
 * list of selected files. Glob metacharacters in names are escaped by `filters()`, so a file
 * called `[draft] *.txt` selects exactly itself either way.
 */
export function selectionToSpec(model: TreeModel, maxGlobs = MAX_GLOBS): SelectionSpec {
  const f = model.filters();
  if (f.include.length + f.exclude.length <= maxGlobs) return { ...f, files: null };
  return { include: [], exclude: [], files: model.selectedFiles() };
}

/**
 * Rows of a filtered tree: only `visible` nodes (matching files and the folders leading to them),
 * every folder open unless it is flagged in `collapsed`.
 */
export function filteredRows(model: TreeModel, visible: Uint8Array, collapsed: Uint8Array): Int32Array {
  const out = new Int32Array(model.n);
  let count = 0;
  let i = 0;
  while (i < model.n) {
    if (!visible[i]) {
      i = model.isDir[i] ? model.end[i] : i + 1;
      continue;
    }
    out[count++] = i;
    i = model.isDir[i] && collapsed[i] ? model.end[i] : i + 1;
  }
  return out.subarray(0, count);
}
