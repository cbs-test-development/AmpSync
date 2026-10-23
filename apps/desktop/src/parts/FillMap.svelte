<script lang="ts">
  import { onMount } from "svelte";
  import {
    bucket,
    cellCount,
    cellInfo,
    cellOfRange,
    MAX_STRIP_CELLS,
    mapSummary,
    type Cells,
    type RangeMap,
  } from "../core/fillmap";

  /**
   * The fill map of one big file: a cell per range, bucketed (see core/fillmap.ts). `strip` is the
   * thin one-row version under a lane (at most 120 cells); `grid` is the larger map of the Selected
   * file panel. The map is plain data that changes without Svelte noticing, so `tick` (bumped once
   * per animation frame by the store) is what triggers a redraw. Cells: empty (the line colour),
   * in flight (dim signal), saved (signal, filled from the bottom when only some of a cell's
   * ranges are in), retried (outline). A range that just finished flashes briefly unless the
   * system asks for reduced motion.
   */
  let {
    map,
    tick = 0,
    flights = [],
    mode = "strip",
  }: { map: RangeMap | undefined; tick?: number; flights?: number[]; mode?: "strip" | "grid" } = $props();

  const PITCH = 12; // grid: cell plus gap
  const GAP = 2;
  const FLASH_MS = 700;

  let canvas: HTMLCanvasElement | undefined = $state();
  let width = $state(0);
  let height = $state(0);
  let tip = $state<{ x: number; y: number; title: string; detail: string } | null>(null);

  interface Layout {
    n: number;
    cols: number;
    cw: number;
    ch: number;
    gap: number;
  }
  let cells: Cells | undefined;
  let layout: Layout | undefined;
  let pal = { empty: "#2a313b", signal: "#f0a43a", fg: "#d8dde5" };
  let reduced = false;
  let raf = 0;

  function css(name: string): string {
    return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  }

  function readPalette() {
    pal = { empty: css("--line") || pal.empty, signal: css("--signal") || pal.signal, fg: css("--fg") || pal.fg };
  }

  function layoutFor(count: number): Layout {
    if (mode === "strip") {
      const n = cellCount(count, Math.min(MAX_STRIP_CELLS, Math.floor(width / 3)));
      const gap = n > 0 && width / n >= 4 ? 1 : 0;
      return { n, cols: n, cw: n > 0 ? (width - gap * (n - 1)) / n : 0, ch: height, gap };
    }
    const maxCols = Math.max(1, Math.floor((width + GAP) / PITCH));
    const maxRows = Math.max(1, Math.floor((height + GAP) / PITCH));
    const n = cellCount(count, maxCols * maxRows);
    return { n, cols: Math.min(maxCols, Math.max(1, n)), cw: PITCH - GAP, ch: PITCH - GAP, gap: GAP };
  }

  function cellAt(l: Layout, c: number): [number, number] {
    const col = c % l.cols;
    const row = Math.floor(c / l.cols);
    return [col * (l.cw + l.gap), row * (l.ch + l.gap)];
  }

  function draw() {
    raf = 0;
    if (!canvas || width === 0 || height === 0) return;
    const dpr = devicePixelRatio || 1;
    const w = Math.round(width * dpr);
    const h = Math.round(height * dpr);
    if (canvas.width !== w) canvas.width = w;
    if (canvas.height !== h) canvas.height = h;
    const g = canvas.getContext("2d");
    if (!g) return;
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    g.clearRect(0, 0, width, height);
    if (!map || map.count === 0) {
      cells = layout = undefined;
      return;
    }
    const l = (layout = layoutFor(map.count));
    const b = (cells = bucket(map, l.n, flights));
    const now = performance.now();
    // Flash: cells that got a finished range in the last moments. Not drawn at all under reduced motion.
    let flash: Float32Array | undefined;
    if (!reduced && map.recent.length > 0) {
      for (const r of map.recent) {
        const age = now - r.t;
        if (age < FLASH_MS) {
          flash ??= new Float32Array(l.n);
          const c = cellOfRange(map.count, l.n, r.i);
          flash[c] = Math.max(flash[c], 1 - age / FLASH_MS);
        }
      }
    }
    for (let c = 0; c < b.n; c++) {
      const [fx, y] = cellAt(l, c);
      // Whole pixels, so neighbouring cells keep an even gap.
      const x = Math.round(fx);
      const cw = Math.max(1, Math.round(fx + l.cw) - x);
      g.fillStyle = pal.empty;
      g.fillRect(x, y, cw, l.ch);
      const frac = b.done[c] / b.total[c];
      const filled = frac >= 1 ? l.ch : frac > 0 ? Math.max(1, Math.round(l.ch * frac)) : 0;
      g.fillStyle = pal.signal;
      if (b.flight[c] > 0 && frac < 1) {
        g.globalAlpha = 0.4;
        g.fillRect(x, y, cw, l.ch - filled);
        g.globalAlpha = 1;
      }
      if (filled > 0) g.fillRect(x, y + l.ch - filled, cw, filled);
      if (b.retried[c] && frac < 1) {
        g.strokeStyle = pal.signal;
        g.lineWidth = 1;
        g.strokeRect(x + 0.5, y + 0.5, Math.max(1, cw - 1), Math.max(1, l.ch - 1));
      }
      if (flash && flash[c] > 0) {
        g.globalAlpha = 0.6 * flash[c];
        g.fillStyle = pal.fg;
        g.fillRect(x, y, cw, l.ch);
        g.globalAlpha = 1;
      }
    }
    if (flash) raf = requestAnimationFrame(draw);
  }

  $effect(() => {
    void tick;
    void flights.join(",");
    void map;
    void width;
    void height;
    void canvas;
    if (!raf) draw();
  });

  function pointer(e: PointerEvent) {
    if (!canvas || !map || !layout || !cells) return (tip = null);
    const r = canvas.getBoundingClientRect();
    const x = e.clientX - r.left;
    const y = e.clientY - r.top;
    const l = layout;
    const col = Math.min(l.cols - 1, Math.max(0, Math.floor(x / (l.cw + l.gap))));
    const row = Math.max(0, Math.floor(y / (l.ch + l.gap)));
    const c = row * l.cols + col;
    if (y < 0 || y > r.height || c >= cells.n) return (tip = null);
    const info = cellInfo(map, cells, c);
    tip = { x: Math.min(e.clientX + 12, innerWidth - 270), y: e.clientY, title: info.title, detail: info.detail };
  }

  onMount(() => {
    reduced = matchMedia("(prefers-reduced-motion: reduce)").matches;
    readPalette();
    const ro = new ResizeObserver((entries) => {
      width = Math.floor(entries[0].contentRect.width);
      height = Math.floor(entries[0].contentRect.height);
    });
    if (canvas) ro.observe(canvas);
    const redraw = () => {
      readPalette();
      draw();
    };
    const dark = matchMedia("(prefers-color-scheme: dark)");
    dark.addEventListener("change", redraw);
    const mo = new MutationObserver(redraw);
    mo.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    return () => {
      ro.disconnect();
      dark.removeEventListener("change", redraw);
      mo.disconnect();
      if (raf) cancelAnimationFrame(raf);
    };
  });

  const summary = $derived(map ? mapSummary(map, flights) : "");
</script>

<div class="fm" class:grid={mode === "grid"} role="img" aria-label={summary ? `Fill map: ${summary}` : "Fill map"}>
  <canvas bind:this={canvas} aria-hidden="true" onpointermove={pointer} onpointerleave={() => (tip = null)}></canvas>
</div>
{#if tip}
  <div class="tip" role="tooltip" style:left="{tip.x}px" style:top="{Math.max(6, tip.y - 52)}px">
    <b>{tip.title}</b>
    <span>{tip.detail}</span>
  </div>
{/if}

<style>
  .fm { position: relative; height: var(--fm-h, 8px); min-width: 0; }
  .fm.grid { height: 100%; min-height: 48px; }
  canvas { position: absolute; inset: 0; display: block; width: 100%; height: 100%; }
  .tip {
    position: fixed; z-index: 30; pointer-events: none; display: grid; gap: 2px; padding: 6px 9px; max-width: 260px;
    background: var(--raise); border: 1px solid var(--line); border-radius: 3px; box-shadow: 0 4px 14px rgb(0 0 0 / .35);
    font: 12px/1.35 var(--f-mono); color: var(--fg);
  }
  .tip b { font-weight: 500; }
  .tip span { color: var(--muted); }
</style>
