<script lang="ts">
  import { untrack } from "svelte";
  import { num, size } from "../core/format";
  import { type CheckState, type TreeModel, visibleRows } from "../core/selection";

  /**
   * The folder picker: a collapsible tree with tri-state checkboxes. Only the rows in view are
   * rendered (fixed row height, absolute positioning), so a folder with 100k files is as light
   * as one with ten. Keyboard: Up/Down move, Right/Left expand/collapse (or step into/out of a
   * folder), Space ticks, Home/End jump.
   */
  let { model, onchange }: { model: TreeModel; onchange: () => void } = $props();

  const ROW = 28;
  const MAX_HEIGHT = 460;
  const OVERSCAN = 6;

  /** Every folder starts collapsed: only the top-level entries show until the user opens one. */
  const expanded = untrack(() => new Uint8Array(model.n));
  let rev = $state(0);
  let scrollTop = $state(0);
  let viewport = $state(MAX_HEIGHT);
  let focused = $state(0); // a node index, not a row number
  let box: HTMLDivElement | undefined = $state();

  const rows = $derived.by(() => {
    void rev;
    return visibleRows(model, expanded);
  });

  /** Row number of a node in `rows`, or the nearest row above it (rows are ascending). */
  function rowOf(node: number): number {
    let lo = 0;
    let hi = rows.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (rows[mid] <= node) lo = mid;
      else hi = mid - 1;
    }
    return lo;
  }

  interface Row {
    i: number;
    top: number;
    name: string;
    path: string;
    depth: number;
    st: CheckState;
    kids: boolean;
    open: boolean;
    count: string;
    size: string;
    isFocus: boolean;
  }

  const view = $derived.by((): Row[] => {
    void rev;
    const first = Math.max(0, Math.floor(scrollTop / ROW) - OVERSCAN);
    const last = Math.min(rows.length, Math.ceil((scrollTop + viewport) / ROW) + OVERSCAN);
    const out: Row[] = [];
    for (let k = first; k < last; k++) {
      const i = rows[k];
      const st = model.state(i);
      const dir = model.isDir[i] === 1;
      const partial = st === 2;
      out.push({
        i,
        top: k * ROW,
        name: model.names[i],
        path: model.path(i),
        depth: model.depth[i],
        st,
        kids: model.hasChildren(i),
        open: expanded[i] === 1,
        count: !dir
          ? "file"
          : partial
            ? `${num(model.selFiles[i])} of ${num(model.files[i])} files`
            : `${num(model.files[i])} ${model.files[i] === 1 ? "file" : "files"}`,
        size: size(partial ? model.selBytes[i] : model.bytes[i]),
        isFocus: i === focused,
      });
    }
    return out;
  });

  function bump() {
    rev++;
  }

  function setOpen(i: number, open: boolean) {
    if (!model.hasChildren(i)) return;
    expanded[i] = open ? 1 : 0;
    // Collapsing hides descendants; keep the focus on a row that still exists.
    if (!open && focused > i && focused < model.end[i]) focused = i;
    bump();
  }

  function tick(i: number) {
    model.toggle(i);
    bump();
    onchange();
  }

  function reveal(node: number) {
    if (!box) return;
    const top = rowOf(node) * ROW;
    if (top < box.scrollTop) box.scrollTop = top;
    else if (top + ROW > box.scrollTop + box.clientHeight) box.scrollTop = top + ROW - box.clientHeight;
  }

  function focusRow(k: number) {
    const clamped = Math.max(0, Math.min(rows.length - 1, k));
    focused = rows[clamped];
    reveal(focused);
  }

  function onKey(e: KeyboardEvent) {
    if (rows.length === 0 || e.ctrlKey || e.metaKey || e.altKey) return;
    const k = rowOf(focused);
    const node = rows[k];
    switch (e.key) {
      case "ArrowDown":
        focusRow(k + 1);
        break;
      case "ArrowUp":
        focusRow(k - 1);
        break;
      case "Home":
        focusRow(0);
        break;
      case "End":
        focusRow(rows.length - 1);
        break;
      case "PageDown":
        focusRow(k + Math.max(1, Math.floor(viewport / ROW) - 1));
        break;
      case "PageUp":
        focusRow(k - Math.max(1, Math.floor(viewport / ROW) - 1));
        break;
      case "ArrowRight":
        if (model.hasChildren(node)) {
          if (!expanded[node]) setOpen(node, true);
          else focusRow(k + 1);
        }
        break;
      case "ArrowLeft":
        if (model.hasChildren(node) && expanded[node]) setOpen(node, false);
        else if (model.parent[node] >= 0) {
          focused = model.parent[node];
          reveal(focused);
        }
        break;
      case " ":
        tick(node);
        break;
      default:
        return;
    }
    e.preventDefault();
  }

  function onScroll() {
    if (box) scrollTop = box.scrollTop;
  }
</script>

<div
  class="tree"
  role="tree"
  tabindex="0"
  aria-label="Folders and files to download"
  aria-multiselectable="true"
  aria-activedescendant={rows.length > 0 ? `ft-${focused}` : undefined}
  style:height="{Math.min(rows.length * ROW, MAX_HEIGHT)}px"
  bind:this={box}
  bind:clientHeight={viewport}
  onscroll={onScroll}
  onkeydown={onKey}
>
  <div class="spacer" style:height="{rows.length * ROW}px">
    {#each view as r (r.i)}
      <div
        class="node"
        class:off={r.st === 0}
        class:focus={r.isFocus}
        id="ft-{r.i}"
        role="treeitem"
        tabindex="-1"
        aria-level={r.depth + 1}
        aria-checked={r.st === 2 ? "mixed" : r.st === 1}
        aria-selected={r.isFocus}
        aria-expanded={r.kids ? r.open : undefined}
        style:top="{r.top}px"
        style:--indent="{r.depth * 22}px"
        onmousedown={() => (focused = r.i)}
      >
        <div class="lead">
          {#if r.kids}
            <button
              type="button"
              class="caret"
              tabindex="-1"
              aria-label={r.open ? `Collapse ${r.name}` : `Expand ${r.name}`}
              onclick={() => setOpen(r.i, !r.open)}>{r.open ? "▾" : "▸"}</button
            >
          {:else}
            <span class="caret-gap"></span>
          {/if}
          <label>
            <input
              type="checkbox"
              tabindex="-1"
              checked={r.st === 1}
              indeterminate={r.st === 2}
              onchange={() => tick(r.i)}
            />
            <span title={r.path}>{r.name}</span>
          </label>
        </div>
        <span class="count">{r.count}</span>
        <span class="size">{r.size}</span>
      </div>
    {/each}
  </div>
</div>

<style>
  .tree { position: relative; overflow-y: auto; font: 13px var(--f-mono); outline: none; }
  .tree:focus-visible { box-shadow: inset 0 0 0 1px var(--signal); }
  .spacer { position: relative; }
  .node { position: absolute; left: 0; right: 0; height: 28px; box-sizing: border-box; display: grid; grid-template-columns: minmax(0, 1fr) auto auto; gap: 14px; align-items: center; padding: 0 14px; }
  .node:hover { background: var(--raise); }
  .tree:focus-visible .node.focus { background: var(--raise); box-shadow: inset 2px 0 0 var(--signal); }
  .lead { display: flex; align-items: center; min-width: 0; padding-left: var(--indent); }
  .caret, .caret-gap { width: 22px; flex: none; }
  .caret { background: none; border: 0; color: var(--muted); cursor: pointer; padding: 2px 0; font-size: 12px; }
  .caret:hover { color: var(--fg); }
  label { display: flex; align-items: center; gap: 8px; min-width: 0; cursor: pointer; }
  label span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .count, .size { color: var(--muted); font-size: 12px; text-align: right; font-variant-numeric: tabular-nums; white-space: nowrap; }
  .size { min-width: 80px; }
  input[type="checkbox"] { accent-color: var(--signal); width: 14px; height: 14px; margin: 0; flex: none; }
  .node.off label span { color: var(--muted); text-decoration: line-through; text-decoration-color: var(--line); }
</style>
