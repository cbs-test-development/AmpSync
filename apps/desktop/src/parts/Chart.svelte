<script lang="ts">
  import { onMount } from "svelte";
  import type { HistSample } from "../core/store.svelte";
  import { markLabel, markText, type LaneMark } from "../core/lanes";

  let { hist }: { hist: HistSample[] } = $props();

  let canvas: HTMLCanvasElement;
  let width = $state(0);
  let H = $state(170);
  const MB = 1024 * 1024;
  const padL = 40, padR = 10, padT = 12, padB = 22;

  /** The lane-count marker under the pointer, if any (F4). */
  let hover = $state<{ x: number; marks: LaneMark[]; ago: number } | null>(null);

  function css(name: string): string {
    return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  }

  function niceMax(peak: number): number {
    const steps = [1, 2, 5, 10, 20, 25, 50, 100, 125, 200, 250, 500, 1000, 2000, 5000];
    return steps.find((s) => s >= peak * 1.1) ?? Math.ceil(peak * 1.1);
  }

  function draw() {
    if (!canvas || width === 0) return;
    const dpr = devicePixelRatio || 1;
    canvas.width = Math.round(width * dpr);
    canvas.height = Math.round(H * dpr);
    const g = canvas.getContext("2d");
    if (!g) return;
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    g.clearRect(0, 0, width, H);
    const values = hist.map((s) => s.v / MB);
    const peak = Math.max(0, ...values);
    const max = niceMax(peak);
    const n = hist.length;
    const x = (i: number) => padL + (i / Math.max(1, n - 1)) * (width - padL - padR);
    const y = (v: number) => padT + (1 - v / max) * (H - padT - padB);
    const muted = css("--muted"), grid = css("--grid"), sig = css("--signal"), info = css("--info");
    g.font = "11px " + css("--f-mono");
    g.fillStyle = muted;
    g.strokeStyle = grid;
    g.lineWidth = 1;
    for (let k = 0; k <= 4; k++) {
      const v = (max / 4) * k;
      const yy = Math.round(y(v)) + 0.5;
      g.beginPath(); g.moveTo(padL, yy); g.lineTo(width - padR, yy); g.stroke();
      g.fillText(String(Math.round(v * 10) / 10), 6, y(v) + 4);
    }
    g.fillText("-2m", padL, H - 6);
    g.fillText("now", width - padR - 22, H - 6);
    g.fillText("MB/s", 6, H - 6);

    // Throttle marker: SharePoint asked AmpSync to slow down.
    let i = 0;
    while (i < n) {
      if (!hist[i].throttled) { i++; continue; }
      let j = i;
      while (j + 1 < n && hist[j + 1].throttled) j++;
      const x0 = x(i) - 2, x1 = x(j) + 2;
      g.fillStyle = info; g.globalAlpha = 0.15;
      g.fillRect(x0, padT, x1 - x0, H - padT - padB);
      g.globalAlpha = 1;
      g.fillStyle = muted;
      g.fillText("429", x0 + 6, padT + 10);
      i = j + 1;
    }

    // Lane markers: a dotted line where automatic tuning changed the lane count, with the new count.
    for (let k = 0; k < n; k++) {
      const marks = hist[k].marks;
      if (!marks) continue;
      const mx = Math.round(x(k)) + 0.5;
      const active = hover !== null && Math.abs(hover.x - x(k)) < 1;
      g.save();
      g.strokeStyle = active ? sig : muted;
      g.globalAlpha = active ? 1 : 0.75;
      g.setLineDash([2, 3]);
      g.lineWidth = 1;
      g.beginPath(); g.moveTo(mx, padT); g.lineTo(mx, H - padB); g.stroke();
      g.restore();
      // A small diamond on the baseline, so the marker can be found even when the line is under the curve.
      g.fillStyle = active ? sig : muted;
      g.beginPath();
      g.moveTo(mx, H - padB - 6); g.lineTo(mx + 4, H - padB - 2); g.lineTo(mx, H - padB + 2); g.lineTo(mx - 4, H - padB - 2);
      g.closePath(); g.fill();
      g.fillStyle = active ? sig : muted;
      g.fillText(markLabel(marks[marks.length - 1]), mx + 5, padT + 10 + (hist[k].throttled ? 12 : 0));
    }

    g.beginPath();
    g.moveTo(x(0), y(0));
    values.forEach((v, k) => g.lineTo(x(k), y(v)));
    g.lineTo(x(n - 1), y(0));
    g.closePath();
    g.fillStyle = sig; g.globalAlpha = 0.14; g.fill(); g.globalAlpha = 1;
    g.beginPath();
    values.forEach((v, k) => (k ? g.lineTo(x(k), y(v)) : g.moveTo(x(k), y(v))));
    g.strokeStyle = sig; g.lineWidth = 1.6; g.stroke();
    const last = values[n - 1] ?? 0;
    g.beginPath(); g.arc(x(n - 1), y(last), 3.5, 0, Math.PI * 2); g.fillStyle = sig; g.fill();
  }

  $effect(() => {
    // Redraw whenever a sample changes or the size changes.
    hist.forEach((s) => s.v + (s.throttled ? 1 : 0) + (s.marks?.length ?? 0));
    void width;
    void H;
    void hover;
    draw();
  });

  function onMove(e: PointerEvent) {
    const n = hist.length;
    if (!canvas || width === 0 || n < 2) return;
    const px = e.clientX - canvas.getBoundingClientRect().left;
    const at = ((px - padL) / (width - padL - padR)) * (n - 1);
    // Markers are a pixel wide, so accept the nearest one within a few samples.
    let best = -1;
    for (let k = Math.max(0, Math.floor(at) - 2); k <= Math.min(n - 1, Math.ceil(at) + 2); k++) {
      if (hist[k].marks && (best < 0 || Math.abs(k - at) < Math.abs(best - at))) best = k;
    }
    if (best < 0 || Math.abs(best - at) > 2.5) {
      hover = null;
      return;
    }
    hover = {
      x: padL + (best / Math.max(1, n - 1)) * (width - padL - padR),
      marks: hist[best].marks ?? [],
      ago: n - 1 - best,
    };
  }

  function agoText(s: number): string {
    return s < 2 ? "just now" : s < 90 ? `${s} s ago` : `${Math.round(s / 60)} min ago`;
  }

  /** The same changes as text, for people who cannot hover. */
  const changes = $derived(
    hist.flatMap((s) => s.marks ?? []).slice(-5).map((m) => markText(m)),
  );

  onMount(() => {
    const ro = new ResizeObserver((entries) => {
      width = Math.floor(entries[0].contentRect.width);
      H = Math.max(80, Math.floor(entries[0].contentRect.height));
    });
    ro.observe(canvas);
    const mq = matchMedia("(prefers-color-scheme: dark)");
    const redraw = () => draw();
    mq.addEventListener("change", redraw);
    const mo = new MutationObserver(redraw);
    mo.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    return () => {
      ro.disconnect();
      mq.removeEventListener("change", redraw);
      mo.disconnect();
    };
  });
</script>

<!-- The wrapper sets the size; the canvas is absolutely placed so its pixel size never feeds back into layout. -->
<div class="wrap" onpointermove={onMove} onpointerleave={() => (hover = null)} role="presentation">
  <canvas bind:this={canvas} aria-label="Download speed over the last two minutes"></canvas>
  {#if hover}
    <div class="tip" role="tooltip" style:left="{Math.min(Math.max(4, hover.x - 120), Math.max(4, width - 244))}px">
      {#each hover.marks as m}
        {@const t = markText(m)}
        <b>{t.title}</b>
        <span>{t.why}</span>
      {/each}
      <i>{agoText(hover.ago)}</i>
    </div>
  {/if}
  {#if changes.length > 0}
    <ul class="sr-only" aria-label="Recent changes to the number of lanes">
      {#each changes as c}<li>{c.title}. {c.why}</li>{/each}
    </ul>
  {/if}
</div>

<style>
  .wrap { position: relative; flex: 1 1 0; min-height: 150px; }
  canvas { position: absolute; inset: 0; display: block; width: 100%; height: 100%; }
  .tip {
    position: absolute; top: 26px; width: 240px; display: grid; gap: 2px; padding: 7px 10px; pointer-events: none;
    background: var(--raise); border: 1px solid var(--line); border-radius: 3px; box-shadow: 0 4px 14px rgb(0 0 0 / .3);
    font-size: 12px; line-height: 1.4;
  }
  .tip b { font: 500 12px var(--f-mono); color: var(--fg); }
  .tip span { color: var(--muted); }
  .tip i { font: normal 11px var(--f-mono); color: var(--muted); opacity: .8; }
  .tip b:not(:first-child) { margin-top: 4px; }
</style>
