import { describe, expect, it } from "vitest";
import {
  bucket,
  cellBounds,
  cellCount,
  cellInfo,
  cellOfRange,
  cellState,
  inFlight,
  inferChunk,
  isRanged,
  mapSummary,
  MAX_STRIP_CELLS,
  mibText,
  RangeMap,
  rangeCount,
} from "./fillmap";

const MiB = 1024 * 1024;
const GB = 1024 * MiB;

function mapOf(count: number, chunk = 16 * MiB, size = count * chunk): RangeMap {
  return new RangeMap(size, count, chunk);
}

describe("which files get a map", () => {
  it("follows the engine rule: strictly above the threshold", () => {
    expect(isRanged(64 * MiB, 64 * MiB)).toBe(false);
    expect(isRanged(64 * MiB + 1, 64 * MiB)).toBe(true);
  });

  it("counts ranges like the engine", () => {
    expect(rangeCount(80 * GB, 16 * MiB)).toBe(5120);
    expect(rangeCount(100 * MiB, 16 * MiB)).toBe(7);
  });

  it("recognises the range size from the count", () => {
    const size = 100 * MiB;
    expect(inferChunk(size, 7, [16 * MiB, 32 * MiB])).toBe(16 * MiB);
    // Budget mode doubles the range size for files started later.
    expect(inferChunk(size, 4, [16 * MiB, 32 * MiB])).toBe(32 * MiB);
    // Neither fits (settings changed meanwhile): the smallest size consistent with the count.
    expect(inferChunk(size, 10, [16 * MiB, 32 * MiB])).toBe(Math.ceil(size / 10));
    expect(inferChunk(0, 0, [16 * MiB])).toBe(16 * MiB);
  });
});

describe("bucketing ranges into cells", () => {
  it("never draws more cells than ranges or than fit", () => {
    expect(cellCount(5, 120)).toBe(5);
    expect(cellCount(5021, MAX_STRIP_CELLS)).toBe(120);
    expect(cellCount(0, 120)).toBe(0);
  });

  it("tiles the ranges exactly: no gap, no overlap, no empty cell", () => {
    for (const [ranges, cells] of [[5021, 120], [7, 7], [8, 3], [121, 120], [1, 1], [100000, 120]]) {
      let next = 0;
      for (let c = 0; c < cells; c++) {
        const [from, to] = cellBounds(ranges, cells, c);
        expect(from).toBe(next);
        expect(to).toBeGreaterThan(from);
        next = to;
      }
      expect(next).toBe(ranges);
    }
  });

  it("finds the cell of any range, the inverse of the bounds", () => {
    for (const [ranges, cells] of [[5021, 120], [97, 40], [120, 120], [13, 5]]) {
      for (let r = 0; r < ranges; r++) {
        const c = cellOfRange(ranges, cells, r);
        const [from, to] = cellBounds(ranges, cells, c);
        expect(r).toBeGreaterThanOrEqual(from);
        expect(r).toBeLessThan(to);
      }
    }
  });

  it("counts done ranges per cell and keeps the total", () => {
    const m = mapOf(5021);
    for (let i = 0; i < 1000; i++) m.markDone(i);
    const cells = bucket(m, 120);
    expect(cells.n).toBe(120);
    expect(cells.done.reduce((a, b) => a + b, 0)).toBe(1000);
    expect(cells.total.reduce((a, b) => a + b, 0)).toBe(5021);
    expect(cellState(cells, 0)).toBe("done");
    expect(cellState(cells, 119)).toBe("empty");
    // The cell where the finished run ends is part way.
    const edge = cellOfRange(5021, 120, 999);
    expect(["done", "partial"]).toContain(cellState(cells, edge));
  });

  it("gives a small file one cell per range", () => {
    const m = mapOf(7);
    m.markDone(2);
    const cells = bucket(m, 120);
    expect(cells.n).toBe(7);
    expect(Array.from({ length: 7 }, (_, c) => cellState(cells, c))).toEqual([
      "empty", "empty", "done", "empty", "empty", "empty", "empty",
    ]);
  });

  it("marks ranges in flight, but not ones that are already done", () => {
    const m = mapOf(10);
    m.markDone(3);
    const cells = bucket(m, 10, [3, 4, 9]);
    expect(cellState(cells, 3)).toBe("done");
    expect(cellState(cells, 4)).toBe("flight");
    expect(cellState(cells, 9)).toBe("flight");
    expect(cellState(cells, 0)).toBe("empty");
  });

  it("outlines retried ranges until they are done", () => {
    const m = mapOf(10);
    m.markRetried(5);
    m.markRetried(6);
    m.markDone(6);
    const cells = bucket(m, 10);
    expect(cells.retried[5]).toBe(1);
    expect(cells.retried[6]).toBe(0);
    expect(cellState(cells, 5)).toBe("empty");
  });

  it("handles a map with no ranges", () => {
    const cells = bucket(mapOf(0), 120);
    expect(cells.n).toBe(0);
  });

  it("is quick enough to run every frame for a huge file", () => {
    const m = mapOf(65536);
    for (let i = 0; i < 30000; i++) m.markDone(i);
    const t = performance.now();
    for (let k = 0; k < 20; k++) bucket(m, 120, [30001, 30002]);
    expect((performance.now() - t) / 20).toBeLessThan(25);
  });
});

describe("range map state", () => {
  it("counts each range once", () => {
    const m = mapOf(4);
    expect(m.markDone(1)).toBe(true);
    expect(m.markDone(1)).toBe(false);
    expect(m.markDone(9)).toBe(false);
    expect(m.markDone(-1)).toBe(false);
    expect(m.doneCount).toBe(1);
  });

  it("starts filled where a resumed download left off", () => {
    const m = mapOf(8);
    for (const i of [0, 1, 2, 5]) m.markDone(i);
    expect(m.doneCount).toBe(4);
    expect(Array.from(bucket(m, 8).done).join("")).toBe("11100100");
  });

  it("starts over when the file is fetched again", () => {
    const m = mapOf(4);
    m.markDone(0);
    m.markRetried(2);
    m.reset();
    expect(m.doneCount).toBe(0);
    expect(m.isRetried(2)).toBe(false);
  });

  it("fills completely when the file is verified", () => {
    const m = mapOf(4);
    m.markRetried(1);
    m.complete();
    expect(m.doneCount).toBe(4);
    expect(cellState(bucket(m, 4), 1)).toBe("done");
  });

  it("keeps finished ranges when the count turns out different, only for the same range size", () => {
    const m = mapOf(10);
    m.markDone(1);
    m.markDone(8);
    const same = m.resized(9, m.chunk);
    expect([same.isDone(1), same.isDone(8)]).toEqual([true, true]);
    const fewer = m.resized(5, m.chunk);
    expect([fewer.isDone(1), fewer.doneCount]).toEqual([true, 1]);
    const other = m.resized(5, m.chunk * 2);
    expect(other.doneCount).toBe(0);
  });

  it("measures the short last range", () => {
    const m = new RangeMap(40 * MiB, 3, 16 * MiB);
    expect(m.rangeBytes(0)).toBe(16 * MiB);
    expect(m.rangeBytes(2)).toBe(8 * MiB);
  });

  it("remembers recent fills only when asked to (for the flash)", () => {
    const m = mapOf(4);
    m.markDone(0);
    m.markDone(1, 1234);
    expect(m.recent).toEqual([{ i: 1, t: 1234 }]);
  });
});

describe("ranges the lanes are working on", () => {
  it("comes from lane offsets", () => {
    const m = mapOf(10);
    expect(inFlight(m, [0, 16 * MiB + 5, 16 * MiB + 9])).toEqual([0, 1]);
  });

  it("skips finished ranges and nonsense", () => {
    const m = mapOf(10);
    m.markDone(2);
    expect(inFlight(m, [2 * 16 * MiB, -1, NaN])).toEqual([]);
  });

  it("clamps an offset at the very end into the last range", () => {
    const m = mapOf(3);
    expect(inFlight(m, [3 * 16 * MiB])).toEqual([2]);
  });
});

describe("what the hover says", () => {
  it("names a single range with its size", () => {
    const m = mapOf(5021);
    const cells = bucket(m, 5021);
    expect(cellInfo(m, cells, 411).title).toBe("range 412 of 5,021 · 16 MiB");
    expect(cellInfo(m, cells, 411).detail).toBe("waiting");
  });

  it("reports the short last range's real size", () => {
    const m = new RangeMap(40 * MiB, 3, 16 * MiB);
    expect(cellInfo(m, bucket(m, 3), 2).title).toBe("range 3 of 3 · 8 MiB");
  });

  it("describes a cell of many ranges by what is saved in it", () => {
    const m = mapOf(5021);
    for (let i = 0; i < 20; i++) m.markDone(i);
    const cells = bucket(m, 120, [20]);
    const info = cellInfo(m, cells, 0);
    expect(info.title).toBe("ranges 1 to 41 of 5,021 · 16 MiB each");
    expect(info.detail).toBe("20 of 41 saved · 1 downloading");
  });

  it("says when a range is being fetched again", () => {
    const m = mapOf(10);
    m.markRetried(4);
    expect(cellInfo(m, bucket(m, 10, [4]), 4).detail).toBe("downloading again");
    expect(cellInfo(m, bucket(m, 10), 4).detail).toBe("waiting to be fetched again");
  });

  it("writes sizes the way people read them", () => {
    expect(mibText(16 * MiB)).toBe("16 MiB");
    expect(mibText(1.5 * MiB)).toBe("1.5 MiB");
    expect(mibText(512 * 1024)).toBe("512 KiB");
  });

  it("summarises for screen readers", () => {
    const m = mapOf(5021);
    m.markDone(0);
    expect(mapSummary(m, [])).toBe("1 of 5,021 ranges saved");
    expect(mapSummary(m, [3, 4])).toBe("1 of 5,021 ranges saved, 2 downloading");
  });
});
