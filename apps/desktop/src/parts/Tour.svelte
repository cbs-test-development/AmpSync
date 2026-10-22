<script lang="ts">
  import { onMount, tick } from "svelte";
  import { app } from "../core/store.svelte";
  import { steps } from "../core/tour";

  let index = $state(0);
  let box: HTMLDivElement;
  let nextBtn: HTMLButtonElement;
  let hole = $state({ left: 0, top: 0, width: 0, height: 0, center: true });
  let boxPos = $state({ left: 16, top: 16 });
  let lastFocus: Element | null = null;

  const step = $derived(steps[index]);
  const reduce = typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;

  async function show(i: number) {
    index = i;
    app.go(steps[i].view);
    await tick();
    requestAnimationFrame(place);
    nextBtn?.focus();
  }

  function place() {
    const s = steps[index];
    const el = s.target ? document.querySelector<HTMLElement>(s.target) : null;
    const vw = innerWidth, vh = innerHeight;
    const bw = box?.offsetWidth ?? 340, bh = box?.offsetHeight ?? 200, m = 12;
    if (!el || el.offsetParent === null) {
      hole = { left: vw / 2, top: vh / 2, width: 0, height: 0, center: true };
      boxPos = { left: (vw - bw) / 2, top: Math.max(16, (vh - bh) / 2) };
      return;
    }
    el.scrollIntoView({ block: "nearest", behavior: reduce ? "auto" : "smooth" });
    const r = el.getBoundingClientRect(), pad = 6;
    hole = { left: r.left - pad, top: r.top - pad, width: r.width + pad * 2, height: r.height + pad * 2, center: false };
    let left: number, top: number;
    if (r.right + m + bw < vw - 16) { left = r.right + m + pad; top = r.top; }
    else if (r.bottom + m + bh < vh - 16) { left = r.left; top = r.bottom + m + pad; }
    else { left = r.left; top = r.top - bh - m - pad; }
    boxPos = {
      left: Math.min(Math.max(16, left), vw - bw - 16),
      top: Math.min(Math.max(16, top), vh - bh - 16),
    };
  }

  function end(skipped: boolean) {
    void app.endTour(skipped).then(() => (lastFocus as HTMLElement | null)?.focus?.());
  }

  function next() {
    if (index === steps.length - 1) end(false);
    else void show(index + 1);
  }

  function back() {
    if (index > 0) void show(index - 1);
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") { e.preventDefault(); end(true); }
    else if (e.key === "ArrowRight") { e.preventDefault(); next(); }
    else if (e.key === "ArrowLeft") { e.preventDefault(); back(); }
    else if (e.key === "Tab") {
      const f = Array.from(box.querySelectorAll<HTMLElement>("button:not([hidden])"));
      if (f.length === 0) return;
      const first = f[0], last = f[f.length - 1];
      if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
      else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
      else if (!box.contains(document.activeElement)) { e.preventDefault(); first.focus(); }
    }
  }

  onMount(() => {
    lastFocus = document.activeElement;
    void show(0);
    addEventListener("keydown", onKey);
    addEventListener("resize", place);
    return () => {
      removeEventListener("keydown", onKey);
      removeEventListener("resize", place);
    };
  });
</script>

<div class="tour">
  <div
    class="hole"
    class:center={hole.center}
    style:left="{hole.left}px"
    style:top="{hole.top}px"
    style:width="{hole.width}px"
    style:height="{hole.height}px"
  ></div>
  <div class="box" bind:this={box} role="dialog" aria-modal="true" aria-labelledby="tour-title" style:left="{boxPos.left}px" style:top="{boxPos.top}px">
    <div class="row">
      <span class="step">{index + 1} / {steps.length}</span>
      <span class="pips" aria-hidden="true">
        {#each steps as _, j}<i class:on={j <= index}></i>{/each}
      </span>
    </div>
    <h3 id="tour-title">{step.title}</h3>
    <p>{step.body}</p>
    <div class="row">
      <button type="button" class="skip" onclick={() => end(true)}>Skip tour</button>
      <span class="btns">
        <button type="button" class="btn ghost" hidden={index === 0} onclick={back}>Back</button>
        <button type="button" class="btn primary" bind:this={nextBtn} onclick={next}>
          {index === 0 ? "Show me around" : index === steps.length - 1 ? "Done" : "Next"}
        </button>
      </span>
    </div>
  </div>
</div>

<style>
  .hole {
    position: fixed; z-index: 50; border-radius: 4px; pointer-events: none;
    box-shadow: 0 0 0 2px var(--signal), 0 0 0 9999px rgba(6, 8, 11, .62);
    transition: top .25s, left .25s, width .25s, height .25s;
  }
  .hole.center { box-shadow: 0 0 0 9999px rgba(6, 8, 11, .62); width: 0 !important; height: 0 !important; }
  .box {
    position: fixed; z-index: 51; width: min(340px, calc(100vw - 32px)); background: var(--raise);
    border: 1px solid var(--line); border-top: 2px solid var(--signal); border-radius: 4px;
    padding: 14px 16px 12px; box-shadow: 0 12px 32px rgba(0, 0, 0, .35); transition: top .25s, left .25s;
  }
  h3 { margin: 4px 0 6px; font: 600 16px/1.25 var(--f-display); text-wrap: balance; }
  p { margin: 0 0 14px; font-size: 13px; color: var(--muted); }
  .row { display: flex; align-items: center; justify-content: space-between; gap: 10px; }
  .btns { display: flex; gap: 8px; }
  .btns [hidden] { display: none; }
  .step { font: 11px var(--f-mono); color: var(--muted); }
  .pips { display: flex; gap: 4px; }
  .pips i { width: 14px; height: 3px; background: var(--line); border-radius: 1px; }
  .pips i.on { background: var(--signal); }
  .skip {
    background: none; border: 0; padding: 4px 2px; color: var(--muted); cursor: pointer; font-size: 12px;
    text-decoration: underline; text-underline-offset: 3px;
  }
  .skip:hover { color: var(--fg); }
  @media (prefers-reduced-motion: reduce) {
    .hole, .box { transition: none; }
  }
</style>
