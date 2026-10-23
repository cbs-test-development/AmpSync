// The fill map (F6): which byte ranges of a big file are on disk. Pure state and bucketing, no DOM,
// so it can be unit-tested. A file above the large-file threshold is split into ranges that lie on a
// fixed grid of `chunk` bytes (range i is i*chunk .. (i+1)*chunk); the engine reports each finished
// range once (`range_done`). An 80 GB file at 16 MiB has about 5,000 ranges, so ranges are bucketed
// into a handful of cells for display.

const MiB = 1024 * 1024;

/** The widest a lane's strip gets, in cells. */
export const MAX_STRIP_CELLS = 120;

const DONE = 1;
const RETRIED = 2;

/** Whether a file is split into ranges, from what the settings say (the engine rule: size > threshold). */
export function isRanged(size: number, largeFileThreshold: number): boolean {
  return size > largeFileThreshold;
}

/**
 * The range size a file was started with. The engine uses the configured range size, or double
 * that after the request budget was hit (D22); anything else falls back to the smallest size that
 * is consistent with the count the engine reported.
 */
export function inferChunk(size: number, count: number, candidates: number[]): number {
  if (count <= 0 || size <= 0) return candidates[0] ?? MiB;
  for (const c of candidates) if (c > 0 && Math.ceil(size / c) === count) return c;
  return Math.ceil(size / count);
}

/** Range count of a ranged file, as the engine computes it. */
export function rangeCount(size: number, chunk: number): number {
  return chunk > 0 ? Math.ceil(size / chunk) : 0;
}

export class RangeMap {
  readonly size: number;
  readonly chunk: number;
  readonly count: number;
  /** One byte per range: bit 0 done, bit 1 retried. */
  private readonly bits: Uint8Array;
  private done = 0;
  /** Recently finished ranges with the time they finished, for the brief flash when one fills in. */
  readonly recent: { i: number; t: number }[] = [];

  constructor(size: number, count: number, chunk: number) {
    this.size = size;
    this.count = Math.max(0, Math.floor(count));
    this.chunk = chunk;
    this.bits = new Uint8Array(this.count);
  }

  get doneCount(): number {
    return this.done;
  }

  isDone(i: number): boolean {
    return (this.bits[i] & DONE) !== 0;
  }

  isRetried(i: number): boolean {
    return (this.bits[i] & RETRIED) !== 0;
  }

  /** Returns whether the range was not already done. */
  markDone(i: number, now = 0): boolean {
    if (i < 0 || i >= this.count || this.isDone(i)) return false;
    this.bits[i] |= DONE;
    this.done++;
    if (now > 0) {
      this.recent.push({ i, t: now });
      if (this.recent.length > 64) this.recent.shift();
    }
    return true;
  }

  markRetried(i: number): void {
    if (i >= 0 && i < this.count && !this.isDone(i)) this.bits[i] |= RETRIED;
  }

  /** Every range done (the file was verified and renamed). */
  complete(): void {
    this.bits.fill(DONE);
    this.done = this.count;
    this.recent.length = 0;
  }

  /** Forget everything: the whole file is fetched again. */
  reset(): void {
    this.bits.fill(0);
    this.done = 0;
    this.recent.length = 0;
  }

  /** The same file seen with another range count: keep what is still valid. */
  resized(count: number, chunk: number): RangeMap {
    const next = new RangeMap(this.size, count, chunk);
    if (chunk === this.chunk) {
      for (let i = 0; i < Math.min(this.count, next.count); i++) {
        if (this.isDone(i)) next.markDone(i);
        if (this.isRetried(i)) next.markRetried(i);
      }
    }
    return next;
  }

  /** Bytes in range `i` (the last one is usually shorter). */
  rangeBytes(i: number): number {
    const start = i * this.chunk;
    return Math.max(0, Math.min(this.chunk, this.size - start));
  }
}

/** Ranges the lanes are fetching right now, from their offsets in the file. Finished ones are skipped. */
export function inFlight(map: RangeMap, offsets: number[]): number[] {
  const out: number[] = [];
  for (const o of offsets) {
    if (!(o >= 0) || map.chunk <= 0) continue;
    const i = Math.min(map.count - 1, Math.floor(o / map.chunk));
    if (i >= 0 && !map.isDone(i) && !out.includes(i)) out.push(i);
  }
  return out;
}

/** How many cells to draw for `ranges` ranges when at most `max` fit. */
export function cellCount(ranges: number, max: number): number {
  return Math.max(0, Math.min(Math.floor(ranges), Math.floor(max)));
}

/** First range of cell `i`, and one past its last; the cells tile the ranges with no gap or overlap. */
export function cellBounds(ranges: number, cells: number, i: number): [number, number] {
  return [Math.floor((i * ranges) / cells), Math.floor(((i + 1) * ranges) / cells)];
}

/** The cell that holds range `r`. */
export function cellOfRange(ranges: number, cells: number, r: number): number {
  return Math.ceil(((r + 1) * cells) / ranges) - 1;
}

export interface Cells {
  n: number;
  /** Ranges in each cell. */
  total: Uint32Array;
  done: Uint32Array;
  flight: Uint32Array;
  /** 1 when a range of the cell was fetched again and the cell is not finished. */
  retried: Uint8Array;
}

export type CellState = "empty" | "flight" | "partial" | "done";

/** Fold the per-range state into `cells` display cells. */
export function bucket(map: RangeMap, cells: number, flights: readonly number[] = []): Cells {
  const n = cellCount(map.count, cells);
  const out: Cells = {
    n,
    total: new Uint32Array(n),
    done: new Uint32Array(n),
    flight: new Uint32Array(n),
    retried: new Uint8Array(n),
  };
  for (let c = 0; c < n; c++) {
    const [from, to] = cellBounds(map.count, n, c);
    out.total[c] = to - from;
    let done = 0;
    let retried = 0;
    for (let r = from; r < to; r++) {
      if (map.isDone(r)) done++;
      else if (map.isRetried(r)) retried = 1;
    }
    out.done[c] = done;
    out.retried[c] = retried;
  }
  for (const r of flights) {
    if (r >= 0 && r < map.count && !map.isDone(r)) out.flight[cellOfRange(map.count, n, r)]++;
  }
  return out;
}

export function cellState(cells: Cells, c: number): CellState {
  if (cells.done[c] === cells.total[c]) return "done";
  if (cells.flight[c] > 0) return "flight";
  return cells.done[c] > 0 ? "partial" : "empty";
}

function num(n: number): string {
  return n.toLocaleString("en-US");
}

/** "16 MiB", or a rounded size when the chunk is not a whole number of MiB. */
export function mibText(bytes: number): string {
  if (bytes >= MiB) {
    const mib = bytes / MiB;
    return Number.isInteger(mib) ? `${num(mib)} MiB` : `${num(Math.round(mib * 10) / 10)} MiB`;
  }
  return `${num(Math.round(bytes / 1024))} KiB`;
}

export interface CellInfo {
  /** "range 412 of 5,021 · 16 MiB", or "ranges 412 to 453 of 5,021 · 16 MiB each" for a cell of several. */
  title: string;
  /** What state it is in, in plain words. */
  detail: string;
}

export function cellInfo(map: RangeMap, cells: Cells, c: number): CellInfo {
  const [from, to] = cellBounds(map.count, cells.n, c);
  const one = to - from === 1;
  const title = one
    ? `range ${num(from + 1)} of ${num(map.count)} · ${mibText(map.rangeBytes(from))}`
    : `ranges ${num(from + 1)} to ${num(to)} of ${num(map.count)} · ${mibText(map.chunk)} each`;
  const state = cellState(cells, c);
  let detail: string;
  if (one) {
    detail =
      state === "done"
        ? "saved"
        : state === "flight"
          ? cells.retried[c]
            ? "downloading again"
            : "downloading"
          : cells.retried[c]
            ? "waiting to be fetched again"
            : "waiting";
  } else {
    const parts = [`${num(cells.done[c])} of ${num(cells.total[c])} saved`];
    if (cells.flight[c] > 0) parts.push(`${num(cells.flight[c])} downloading`);
    if (cells.retried[c] && state !== "done") parts.push("some fetched again");
    detail = parts.join(" · ");
  }
  return { title, detail };
}

/** A sentence for screen readers and the panel under the big map. */
export function mapSummary(map: RangeMap, flights: readonly number[]): string {
  const base = `${num(map.doneCount)} of ${num(map.count)} ranges saved`;
  return flights.length > 0 ? `${base}, ${num(flights.length)} downloading` : base;
}
