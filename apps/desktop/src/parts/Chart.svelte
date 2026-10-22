<script lang="ts">
  import { onMount } from "svelte";
  import type { HistSample } from "../core/store.svelte";

  let { hist }: { hist: HistSample[] } = $props();

  let canvas: HTMLCanvasElement;
  let width = $state(0);
  let H = $state(170);
  const MB = 1024 * 1024;

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
    const padL = 40, padR = 10, padT = 12, padB = 22;
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
    hist.forEach((s) => s.v + (s.throttled ? 1 : 0));
    void width;
    void H;
    draw();
  });

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

<canvas bind:this={canvas} aria-label="Download speed over the last two minutes"></canvas>

<style>
  canvas { display: block; width: 100%; height: clamp(96px, 17vh, 170px); }
</style>
