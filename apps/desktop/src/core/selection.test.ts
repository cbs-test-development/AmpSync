import { describe, expect, it } from "vitest";
import { buildIndex, emptySpec, runFilter } from "./filter";
import { filteredRows, MAX_GLOBS, selectedAmong, selectionToSpec, TreeModel, visibleRows } from "./selection";
import { treeOf, type TestFile } from "./testkit";

/**
 * Just enough of the engine's glob dialect to check escaping: `**`, `*`, `?` and `[x]` character
 * classes (a lone `]` right after `[` is literal). Case-insensitive like the engine.
 */
function globToRegExp(glob: string): RegExp {
  let out = "";
  for (let i = 0; i < glob.length; i++) {
    const c = glob[i];
    if (c === "*" && glob[i + 1] === "*") {
      out += ".*";
      i++;
    } else if (c === "*") out += "[^/]*";
    else if (c === "?") out += "[^/]";
    else if (c === "[") {
      let j = i + 1;
      if (glob[j] === "]") j++;
      while (j < glob.length && glob[j] !== "]") j++;
      out += `[${glob.slice(i + 1, j).replace(/[\\\]^]/g, "\\$&")}]`;
      i = j;
    } else out += c.replace(/[.+^${}()|\\/]/g, "\\$&");
  }
  return new RegExp(`^${out}$`, "i");
}

/** Which files the engine would download for these globs (exclude wins). */
function engineSelects(paths: string[], include: string[], exclude: string[]): string[] {
  const inc = include.map(globToRegExp);
  const exc = exclude.map(globToRegExp);
  return paths.filter((p) => (inc.length === 0 || inc.some((r) => r.test(p))) && !exc.some((r) => r.test(p)));
}

function modelOf(files: TestFile[]): { model: TreeModel; tree: ReturnType<typeof treeOf> } {
  const tree = treeOf(files);
  return { model: new TreeModel(tree), tree };
}

const idx = (tree: ReturnType<typeof treeOf>, name: string) => tree.names.indexOf(name);

describe("selectionToSpec", () => {
  it("sends no patterns at all when everything is selected", () => {
    const { model } = modelOf([{ path: "a/x.txt" }, { path: "b/y.txt" }]);
    expect(selectionToSpec(model)).toEqual({ include: [], exclude: [], files: null });
  });

  it("uses the shorter compact glob list while it has at most 50 patterns", () => {
    const pad = (i: number) => `d${String(i).padStart(3, "0")}`;
    const files = Array.from({ length: 120 }, (_, i) => ({ path: `${pad(i)}/f.txt` }));
    const { model, tree } = modelOf(files);
    for (let i = 0; i < 50; i++) model.set(idx(tree, pad(i)), false);
    // 50 unticked folders against 70 ticked ones: the exclude form is shorter.
    let s = selectionToSpec(model);
    expect(s.files).toBeNull();
    expect(s.include).toEqual([]);
    expect(s.exclude).toHaveLength(50);
    expect(s.exclude[0]).toBe("d000/**");
    // One more makes the shorter form 51 patterns: the explicit list takes over.
    model.set(idx(tree, pad(50)), false);
    s = selectionToSpec(model);
    expect(s.exclude).toEqual([]);
    expect(s.files).toHaveLength(69);
    // Mostly unticked: the include form is shorter and stays compact.
    const { model: m2, tree: t2 } = modelOf(files);
    for (let i = 0; i < m2.n; i++) m2.set(i, false);
    for (let i = 0; i < 3; i++) m2.set(idx(t2, pad(i)), true);
    expect(selectionToSpec(m2)).toEqual({ include: ["d000/**", "d001/**", "d002/**"], exclude: [], files: null });
  });

  it("falls back to the explicit file list above 50 patterns", () => {
    // 120 folders with one file each; every second folder unticked gives 60 exclude patterns
    // and 60 include patterns, both over the limit.
    const files = Array.from({ length: 120 }, (_, i) => ({ path: `d${String(i).padStart(3, "0")}/f.txt`, size: i + 1 }));
    const { model, tree } = modelOf(files);
    for (let i = 0; i < 120; i += 2) model.set(idx(tree, `d${String(i).padStart(3, "0")}`), false);
    const s = selectionToSpec(model);
    expect(s.include).toEqual([]);
    expect(s.exclude).toEqual([]);
    expect(s.files).toHaveLength(60);
    expect(s.files![0]).toBe("d001/f.txt");
    expect(s.files!.every((p) => p.endsWith("/f.txt"))).toBe(true);
    expect(model.selected.files).toBe(60);
  });

  it("switches exactly at the limit", () => {
    const files = Array.from({ length: 200 }, (_, i) => ({ path: `d${i}/f.txt` }));
    const { model, tree } = modelOf(files);
    for (let i = 0; i < 100; i++) model.set(idx(tree, `d${i}`), false);
    // 100 unticked vs 100 ticked: 100 patterns either way.
    expect(selectionToSpec(model, 100).files).toBeNull();
    expect(selectionToSpec(model, 99).files).toHaveLength(100);
    expect(MAX_GLOBS).toBe(50);
  });

  it("selects exactly the right files when names contain glob characters", () => {
    const names = ["[draft] *.txt", "{a,b}.txt", "what?.txt", "a]b.txt", "plain.txt", "d x.txt", "bb.txt", "a.txt", "ab.txt"];
    const files = names.map((n) => ({ path: `Weird [1]/${n}` })).concat([{ path: "Weird [1]/sub*/in.txt" }, { path: "other/p.txt" }]);
    const { model, tree } = modelOf(files);
    const all = files.map((f) => f.path).sort();
    // Untick one awkward file: a single exclude pattern that must hit that file only.
    model.set(idx(tree, "[draft] *.txt"), false);
    let s = selectionToSpec(model);
    expect(s.files).toBeNull();
    expect(engineSelects(all, s.include, s.exclude)).toEqual(all.filter((p) => p !== "Weird [1]/[draft] *.txt"));
    // Keep only two awkward files: include patterns, each matching its own file.
    for (let i = 0; i < model.n; i++) model.set(i, false);
    model.set(idx(tree, "{a,b}.txt"), true);
    model.set(idx(tree, "what?.txt"), true);
    model.set(idx(tree, "in.txt"), true);
    s = selectionToSpec(model);
    expect(s.files).toBeNull();
    expect(s.include).toHaveLength(3);
    expect(engineSelects(all, s.include, s.exclude).sort()).toEqual(
      ["Weird [1]/sub*/in.txt", "Weird [1]/what?.txt", "Weird [1]/{a,b}.txt"].sort(),
    );
    // Folder names with glob characters are escaped too; the file list keeps raw names.
    expect(s.include.every((p) => p.startsWith("Weird [[]1[]]/"))).toBe(true);
    expect(model.selectedFiles()).toEqual(["Weird [1]/sub*/in.txt", "Weird [1]/what?.txt", "Weird [1]/{a,b}.txt"]);
  });

  it("lists raw names when the explicit list is used", () => {
    const files = Array.from({ length: 120 }, (_, i) => ({ path: `d${i}/[x] f?.txt` }));
    const { model, tree } = modelOf(files);
    for (let i = 0; i < 120; i += 2) model.set(idx(tree, `d${i}`), false);
    const s = selectionToSpec(model);
    expect(s.files).toHaveLength(60);
    expect(s.files![0]).toBe("d1/[x] f?.txt");
  });
});

describe("matching selection", () => {
  const files: TestFile[] = [
    { path: "docs/a.pdf", size: 10 },
    { path: "docs/b.txt", size: 20 },
    { path: "docs/deep/c.pdf", size: 40 },
    { path: "media/d.pdf", size: 80 },
    { path: "media/e.mp4", size: 160 },
  ];

  function setup() {
    const { model, tree } = modelOf(files);
    const f = runFilter(buildIndex(tree), { ...emptySpec(), exts: ["pdf"] });
    return { model, tree, f };
  }

  it("Clear matching unticks only matching files and keeps the totals right", () => {
    const { model, tree, f } = setup();
    model.setMatching(0, model.n, false, f.match);
    expect(model.selected).toEqual({ files: 2, bytes: 180 });
    expect(model.state(idx(tree, "docs"))).toBe(2);
    expect(model.state(idx(tree, "a.pdf"))).toBe(0);
    expect(model.state(idx(tree, "b.txt"))).toBe(1);
    expect(model.selFiles[idx(tree, "media")]).toBe(1);
  });

  it("Select matching ticks them again, without double counting", () => {
    const { model, f } = setup();
    for (let i = 0; i < model.n; i++) model.set(i, false);
    model.setMatching(0, model.n, true, f.match);
    model.setMatching(0, model.n, true, f.match);
    expect(model.selected).toEqual({ files: 3, bytes: 130 });
    model.setMatching(0, model.n, false, f.match);
    expect(model.selected).toEqual({ files: 0, bytes: 0 });
  });

  it("acts on one folder's range only", () => {
    const { model, tree, f } = setup();
    const media = idx(tree, "media");
    model.setMatching(media, model.end[media], false, f.match);
    expect(model.selected).toEqual({ files: 4, bytes: 230 });
    expect(model.state(idx(tree, "a.pdf"))).toBe(1);
  });

  it("counts selected matching files per node", () => {
    const { model, tree, f } = setup();
    model.set(idx(tree, "c.pdf"), false);
    const sel = selectedAmong(model, f.match);
    expect(sel[idx(tree, "docs")]).toBe(1);
    expect(sel[idx(tree, "deep")]).toBe(0);
    expect(sel[idx(tree, "media")]).toBe(1);
  });

  it("lists the rows of a filtered tree, folders open unless collapsed", () => {
    const { model, tree, f } = setup();
    const none = new Uint8Array(model.n);
    const names = (rows: Int32Array) => [...rows].map((i) => tree.names[i]);
    expect(names(filteredRows(model, f.visible, none))).toEqual(["docs", "deep", "c.pdf", "a.pdf", "media", "d.pdf"]);
    const collapsed = new Uint8Array(model.n);
    collapsed[idx(tree, "docs")] = 1;
    expect(names(filteredRows(model, f.visible, collapsed))).toEqual(["docs", "media", "d.pdf"]);
    // The normal tree starts collapsed and is untouched by all of this.
    expect(names(visibleRows(model, new Uint8Array(model.n)))).toEqual(["docs", "media"]);
  });
});
