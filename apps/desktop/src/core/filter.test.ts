import { describe, expect, it } from "vitest";
import {
  buildIndex,
  dayStart,
  emptySpec,
  emptyState,
  extOf,
  extensionStats,
  isActive,
  nameMatcher,
  runFilter,
  splitChips,
  toSpec,
  type FilterSpec,
} from "./filter";
import { FilterRunner } from "./filterRunner";
import { treeOf, type TestFile } from "./testkit";

const MB = 1024 * 1024;
const GB = 1024 * MB;

const spec = (p: Partial<FilterSpec>): FilterSpec => ({ ...emptySpec(), ...p });

function pathOf(names: string[], parents: number[], i: number): string {
  const parts: string[] = [];
  for (let k = i; k >= 0; k = parents[k]) parts.push(names[k]);
  return parts.reverse().join("/");
}

/** Paths of the files a filter matches, in display order. */
function matches(files: TestFile[], p: Partial<FilterSpec>): string[] {
  const tree = treeOf(files);
  const r = runFilter(buildIndex(tree), spec(p));
  const paths: string[] = [];
  tree.names.forEach((_, i) => {
    if (r.match[i]) paths.push(pathOf(tree.names, tree.parents, i));
  });
  return paths;
}

describe("nameMatcher", () => {
  it("is case-insensitive and treats plain text as 'contains'", () => {
    const m = nameMatcher("Report")!;
    expect(m("quarterly report q3.pdf")).toBe(true);
    expect(m("report.pdf")).toBe(true);
    expect(m("summary.pdf")).toBe(false);
    expect(nameMatcher("  ")).toBeNull();
    expect(nameMatcher("")).toBeNull();
  });

  it("supports * and ? wildcards over the whole name", () => {
    const pdf = nameMatcher("*.PDF")!;
    expect(pdf("a.pdf")).toBe(true);
    expect(pdf("a.pdf.txt")).toBe(false);
    const q = nameMatcher("rep?rt*")!;
    expect(q("report final.docx")).toBe(true);
    expect(q("reprt.docx")).toBe(false);
    expect(q("my report")).toBe(false);
    expect(nameMatcher("2023*")!("2023-budget.xlsx")).toBe(true);
    expect(nameMatcher("**a**")!("bab")).toBe(true);
  });

  it("keeps every other character literal", () => {
    const m = nameMatcher("a.b*")!;
    expect(m("a.b.txt")).toBe(true);
    expect(m("axb.txt")).toBe(false);
    expect(nameMatcher("(draft) [v2]*")!("(draft) [v2] x.doc")).toBe(true);
    expect(nameMatcher("a+b?")!("a+bc")).toBe(true);
    expect(nameMatcher("a+b?")!("aab c")).toBe(false);
    // Plain text with regex characters is a substring, not a pattern.
    expect(nameMatcher("a.b")!("axb")).toBe(false);
  });
});

describe("extOf", () => {
  it("lower-cases and ignores dot-files and trailing dots", () => {
    expect(extOf("Plan.PDF")).toBe("pdf");
    expect(extOf("archive.tar.gz")).toBe("gz");
    expect(extOf(".gitignore")).toBe("");
    expect(extOf("name.")).toBe("");
    expect(extOf("README")).toBe("");
  });
});

describe("runFilter", () => {
  const files: TestFile[] = [
    { path: "Reports/2023/Budget.xlsx", size: 5 * MB, modified: 1_700_000_000 },
    { path: "Reports/2023/Summary.pdf", size: 300 * MB, modified: 1_700_100_000 },
    { path: "Reports/2024/Budget.xlsx", size: 6 * MB, modified: 1_730_000_000 },
    { path: "Photos/IMG_0001.JPG", size: 2 * GB, modified: 1_600_000_000 },
    { path: "Photos/IMG_0002.jpg", size: 3 * MB, modified: 1_610_000_000 },
    { path: "notes", size: 10, modified: 1_720_000_000 },
  ];

  it("matches everything when no part of the filter is set", () => {
    expect(matches(files, {})).toHaveLength(6);
    expect(isActive(emptySpec())).toBe(false);
  });

  it("searches names, case-insensitively, with wildcards", () => {
    expect(matches(files, { query: "budget" })).toEqual(["Reports/2023/Budget.xlsx", "Reports/2024/Budget.xlsx"]);
    expect(matches(files, { query: "*.JPG" })).toEqual(["Photos/IMG_0001.JPG", "Photos/IMG_0002.jpg"]);
    expect(matches(files, { query: "img_000?.jpg" })).toHaveLength(2);
  });

  it("lets a folder name match for everything inside it", () => {
    expect(matches(files, { query: "2023" })).toEqual(["Reports/2023/Budget.xlsx", "Reports/2023/Summary.pdf"]);
    expect(matches(files, { query: "2023", exts: ["pdf"] })).toEqual(["Reports/2023/Summary.pdf"]);
  });

  it("filters by extension, with '' meaning no extension", () => {
    expect(matches(files, { exts: ["xlsx"] })).toHaveLength(2);
    expect(matches(files, { exts: ["jpg", "pdf"] })).toHaveLength(3);
    expect(matches(files, { exts: [""] })).toEqual(["notes"]);
  });

  it("filters by size, inclusive at both ends", () => {
    expect(matches(files, { minSize: 100 * MB })).toEqual(["Photos/IMG_0001.JPG", "Reports/2023/Summary.pdf"]);
    expect(matches(files, { minSize: 5 * MB, maxSize: 6 * MB })).toHaveLength(2);
    expect(matches(files, { maxSize: 10 })).toEqual(["notes"]);
  });

  it("filters by modified time", () => {
    expect(matches(files, { since: 1_725_000_000 })).toEqual(["Reports/2024/Budget.xlsx"]);
    expect(matches(files, { since: 1_720_000_000 })).toHaveLength(2);
  });

  it("combines every part with AND", () => {
    expect(matches(files, { query: "budget", exts: ["xlsx"], minSize: 6 * MB, since: 1_700_000_000 })).toEqual([
      "Reports/2024/Budget.xlsx",
    ]);
    expect(matches(files, { query: "budget", minSize: GB })).toEqual([]);
  });

  it("marks matching files and the folders leading to them, with rollups", () => {
    const tree = treeOf(files);
    const r = runFilter(buildIndex(tree), spec({ exts: ["xlsx"] }));
    const at = (name: string) => tree.names.indexOf(name);
    expect(r.count).toBe(2);
    expect(r.totalBytes).toBe(11 * MB);
    expect(r.visible[at("Reports")]).toBe(1);
    expect(r.visible[at("2023")]).toBe(1);
    expect(r.visible[at("Photos")]).toBe(0);
    expect(r.visible[at("Summary.pdf")]).toBe(0);
    expect(r.files[at("Reports")]).toBe(2);
    expect(r.bytes[at("2024")]).toBe(6 * MB);
    expect(r.files[at("Photos")]).toBe(0);
  });

  it("reports nothing for a filter nothing satisfies", () => {
    const r = runFilter(buildIndex(treeOf(files)), spec({ query: "zzz" }));
    expect(r.count).toBe(0);
    expect(r.visible.every((v) => v === 0)).toBe(true);
  });

  it("stays fast on 100k files", () => {
    const many = Array.from({ length: 100_000 }, (_, i) => ({
      path: `d${i % 200}/s${i % 37}/File ${i}.${["pdf", "docx", "mp4", "jpg"][i % 4]}`,
      size: i * 1000,
      modified: 1_600_000_000 + i,
    }));
    const ix = buildIndex(treeOf(many));
    const t0 = performance.now();
    const r = runFilter(ix, spec({ query: "file 9*", exts: ["pdf", "mp4"], since: 1_600_000_500, minSize: 1 }));
    const ms = performance.now() - t0;
    expect(r.count).toBeGreaterThan(0);
    // The target is 50 ms; the bound is loose so a busy CI machine does not fail the build.
    expect(ms).toBeLessThan(300);
  });
});

describe("type chips", () => {
  const tree = treeOf([
    { path: "a/x.mp4", size: 900 },
    { path: "a/y.MP4", size: 100 },
    { path: "b/z.pdf", size: 500 },
    { path: "README", size: 50 },
    ...Array.from({ length: 9 }, (_, i) => ({ path: `c/f${i}.e${i}`, size: 10 + i })),
  ]);

  it("ranks extensions by total size, merging case", () => {
    const stats = extensionStats(tree);
    expect(stats.slice(0, 3).map((s) => [s.key, s.files, s.bytes])).toEqual([
      ["mp4", 2, 1000],
      ["pdf", 1, 500],
      ["", 1, 50],
    ]);
  });

  it("shows the top 8 and leaves the rest for More", () => {
    const all = extensionStats(tree);
    expect(all).toHaveLength(12);
    const { top, more } = splitChips(all);
    expect(top).toHaveLength(8);
    expect(more).toHaveLength(4);
    expect(splitChips([], 8)).toEqual({ top: [], more: [] });
  });
});

describe("toSpec", () => {
  const now = 1_800_000_000;

  it("turns the presets into bounds", () => {
    expect(toSpec(emptyState(), now)).toEqual(emptySpec());
    expect(toSpec({ ...emptyState(), size: "100mb" }, now).minSize).toBe(100 * MB);
    expect(toSpec({ ...emptyState(), size: "1gb" }, now).minSize).toBe(GB);
    expect(toSpec({ ...emptyState(), modified: "7d" }, now).since).toBe(now - 7 * 86400);
    expect(toSpec({ ...emptyState(), modified: "30d" }, now).since).toBe(now - 30 * 86400);
  });

  it("reads a custom size range in the chosen unit and ignores nonsense", () => {
    const s = toSpec({ ...emptyState(), size: "custom", minCustom: "1,5", maxCustom: "2", unit: "GB" }, now);
    expect([s.minSize, s.maxSize]).toEqual([1.5 * GB, 2 * GB]);
    const bad = toSpec({ ...emptyState(), size: "custom", minCustom: "abc", maxCustom: "-3" }, now);
    expect([bad.minSize, bad.maxSize]).toEqual([0, 0]);
  });

  it("uses local midnight for a 'since' date and stays off until one is picked", () => {
    expect(toSpec({ ...emptyState(), modified: "since", sinceDate: "" }, now).since).toBe(0);
    const t = toSpec({ ...emptyState(), modified: "since", sinceDate: "2024-03-09" }, now).since;
    expect(t).toBe(Math.floor(new Date(2024, 2, 9).getTime() / 1000));
    expect(dayStart("yesterday")).toBe(0);
  });

  it("passes the search text and ticked types through", () => {
    const s = toSpec({ ...emptyState(), query: "x*", exts: ["pdf"] }, now);
    expect([s.query, s.exts]).toEqual(["x*", ["pdf"]]);
    expect(isActive(s)).toBe(true);
  });
});

describe("FilterRunner", () => {
  it("filters small listings on the spot", async () => {
    const tree = treeOf([{ path: "a/b.txt" }, { path: "a/c.pdf" }]);
    const runner = new FilterRunner(tree);
    const r = await runner.run(spec({ exts: ["pdf"] }));
    expect(r.count).toBe(1);
    expect(r.match[tree.names.indexOf("c.pdf")]).toBe(1);
    runner.dispose();
  });
});
