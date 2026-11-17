<script lang="ts">
  // The browser link set-up guide: four steps, an Edge/Chrome switch and a small animation per step.
  // It opens from the tour, the sign-in chooser, Sign-ins, Settings and the nudge (`app.openGuide()`).
  import { onMount } from "svelte";
  import { save as saveDialog } from "@tauri-apps/plugin-dialog";
  import { app } from "../core/store.svelte";
  import { api, toCmdError } from "../core/api";
  import { browserName } from "../core/signin";
  import {
    GUIDE_STEPS,
    LAST_STEP,
    developerModeWhere,
    extensionsUrl,
    initialBrowser,
    lede,
    primaryEnabled,
    primaryLabel,
    revealLabel,
    stepMark,
    stepState,
  } from "../core/linkguide";
  import type { LinkBrowser } from "../core/types";

  let step = $state(0);
  let browser = $state<LinkBrowser>(initialBrowser(app.link));
  let copied = $state<"url" | "path" | null>(null);
  let hint = $state("");
  let prepared = $state("");
  let dlg: HTMLDivElement;
  let primary: HTMLButtonElement;
  let lastFocus: Element | null = null;
  let copyTimer: ReturnType<typeof setTimeout> | undefined;

  const name = $derived(browserName(browser));
  const url = $derived(extensionsUrl(browser));
  const folder = $derived(prepared || app.link?.folder || "");
  const connected = $derived(app.link?.connected === true);
  // Reduced motion: the system preference (a media query in the CSS) or the app's own "Reduced
  // motion" startup-animation setting, which is the only motion setting AmpSync has.
  const calm = $derived(app.settings?.splash === "reduced");
  // The animated typing shows at most what fits in the folder picker.
  const folderChars = $derived(Math.max(1, Math.min(folder.length, 38)));

  function go(n: number) {
    step = n;
    copied = null;
    hint = "";
  }

  // The extension attached while the guide was on an earlier step: show the good news.
  let wasConnected: boolean | undefined;
  $effect(() => {
    if (connected && wasConnected === false) go(LAST_STEP);
    wasConnected = connected;
  });

  async function copy(text: string, which: "url" | "path") {
    let ok = false;
    try {
      await navigator.clipboard.writeText(text);
      ok = true;
    } catch {
      // No clipboard permission: fall back to a selection in a throwaway field.
      const t = document.createElement("textarea");
      t.value = text;
      t.setAttribute("readonly", "");
      t.style.cssText = "position:fixed;opacity:0;left:-9999px";
      document.body.appendChild(t);
      t.select();
      try {
        ok = document.execCommand("copy");
      } catch {
        ok = false;
      }
      t.remove();
    }
    if (!ok) {
      app.toast("Could not copy. Select the text and copy it yourself.");
      return;
    }
    copied = which;
    clearTimeout(copyTimer);
    copyTimer = setTimeout(() => (copied = null), 2200);
  }

  async function openExtensions() {
    hint = "";
    let opened = false;
    try {
      opened = await api.linkOpenExtensions(browser);
    } catch {
      opened = false;
    }
    if (opened) go(1);
    else hint = `AmpSync couldn't open ${name}. Copy the address below into ${name}'s address bar, then carry on.`;
  }

  async function reveal() {
    try {
      await api.linkRevealFolder();
    } catch (e) {
      app.toast(toCmdError(e).message);
    }
  }

  async function saveZip() {
    try {
      const dest = await saveDialog({ defaultPath: "ampsync-browser-link.zip", filters: [{ name: "Zip", extensions: ["zip"] }] });
      if (typeof dest !== "string") return;
      await api.linkSaveZip(dest);
      app.toast("Saved the extension zip.");
    } catch (e) {
      app.toast(toCmdError(e).message);
    }
  }

  function next() {
    if (step === 0 && !hint) void openExtensions();
    else if (step === 0) go(1);
    else if (step < LAST_STEP) go(step + 1);
    else if (connected) app.closeGuide();
  }

  function close() {
    app.closeGuide();
    (lastFocus as HTMLElement | null)?.focus?.();
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      close();
    } else if (e.key === "Tab") {
      const f = Array.from(dlg.querySelectorAll<HTMLElement>("button:not(:disabled), input, a[href], select, textarea"));
      if (f.length === 0) return;
      const first = f[0], last = f[f.length - 1];
      if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
      else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
      else if (!dlg.contains(document.activeElement)) { e.preventDefault(); first.focus(); }
    }
  }

  onMount(() => {
    lastFocus = document.activeElement;
    primary?.focus();
    // Extract and register the extension; the folder is shown from step 3 on.
    api.linkPrepare().then((f) => (prepared = f)).catch((e) => app.toast(toCmdError(e).message));
    addEventListener("keydown", onKey);
    return () => {
      removeEventListener("keydown", onKey);
      clearTimeout(copyTimer);
    };
  });
</script>

{#snippet pointer(cls: string, pos: string)}
  <svg class="cursor {cls}" viewBox="0 0 20 20" style={pos} aria-hidden="true"><path d="M3 2l12 8-5 1 3 6-2 1-3-6-4 3z" fill="#fff" stroke="#111" stroke-width="1" /></svg>
{/snippet}

<div class="scrim">
  <div class="guide" class:calm bind:this={dlg} role="dialog" aria-modal="true" aria-labelledby="g-title">
    <aside>
      <div class="pick">
        <span class="label">Browser link · 2 minutes</span>
        <div class="seg" role="group" aria-label="Your browser">
          <button type="button" aria-pressed={browser === "edge"} onclick={() => (browser = "edge")}>Edge</button>
          <button type="button" aria-pressed={browser === "chrome"} onclick={() => (browser = "chrome")}>Chrome</button>
        </div>
      </div>
      <ol class="steps" aria-label="Steps">
        {#each GUIDE_STEPS as title, i}
          <li>
            <button type="button" class="stp {stepState(i, step)}" aria-current={i === step ? "step" : undefined} onclick={() => go(i)}>
              <span class="n">{stepMark(i, step)}</span><span>{title}</span>
            </button>
          </li>
        {/each}
      </ol>
      <div class="lock">
        <svg viewBox="0 0 20 20" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.6" aria-hidden="true"><rect x="4" y="9" width="12" height="8" rx="1.5" /><path d="M7 9V6.5a3 3 0 0 1 6 0V9" /></svg>
        <span>The extension reads only your SharePoint sign-in. It can't see passwords, other sites or your browsing.</span>
      </div>
    </aside>

    <section>
      <div class="head">
        <div>
          <h3 id="g-title">Never sign in to AmpSync again</h3>
          <p class="lede">{lede(browser)}</p>
        </div>
        <button type="button" class="btn ghost close" aria-label="Close" onclick={close}>
          <svg viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.8" aria-hidden="true"><path d="M5 5l10 10M15 5L5 15" /></svg>
        </button>
      </div>

      <div class="ill" aria-hidden="true">
        {#key `${step}-${browser}`}
          {#if step === 0}
            <div class="bw">
              <div class="tabs"><span class="tab">New tab</span></div>
              <div class="addr"><span class="nav">←  →  ⟳</span><span class="box"><span class="typed" style="--n:{url.length}">{url}</span><span class="caret"></span></span></div>
              <div class="skel"><div class="ph" style="width: 60%"></div><div class="ph" style="width: 40%"></div><div class="ph" style="width: 52%"></div></div>
            </div>
          {:else if step === 1}
            <div class="bw">
              <div class="addr"><span class="box">{url}</span></div>
              {#if browser === "edge"}
                <div class="pg">
                  <div class="side" style="position: relative">
                    <strong class="side-h">Extensions</strong>
                    <span class="it sel">Installed extensions</span><span class="it">Keyboard shortcuts</span><span class="it">Get extensions</span>
                    <div class="devrow">Developer mode<span class="tg"></span><span class="ring" style="inset: -3px -4px"></span></div>
                    {@render pointer("cur-a", "left: 112px; bottom: 60px")}
                  </div>
                  <div class="main-pg"><strong>Installed extensions</strong><div class="card"><span class="icon"></span><div class="lines"><div class="ph" style="width: 50%"></div><div class="ph" style="width: 80%"></div></div></div></div>
                </div>
              {:else}
                <div class="chrome-pg">
                  <div class="chrome-h"><strong>Extensions</strong><span class="dev">Developer mode <span class="tg"></span><span class="ring" style="inset: -4px -6px"></span></span></div>
                  <div class="main-pg"><div class="card"><span class="icon"></span><div class="lines"><div class="ph" style="width: 50%"></div><div class="ph" style="width: 80%"></div></div></div></div>
                  {@render pointer("cur-b", "right: 20px; top: 20px")}
                </div>
              {/if}
            </div>
          {:else if step === 2}
            <div class="bw">
              <div class="addr"><span class="box">{url}</span></div>
              <div class="main-pg" style="position: relative">
                <div class="tb"><span class="b hl">Load unpacked<span class="ring" style="inset: -3px"></span></span><span class="b">Pack extension</span><span class="b">Update</span><span class="dev2">Developer mode <span class="tg on"></span></span></div>
                <div class="card"><span class="icon"></span><div class="lines"><div class="ph" style="width: 50%"></div><div class="ph" style="width: 80%"></div></div></div>
                {@render pointer("", "left: 60px; top: 22px")}
              </div>
              <div class="picker">
                <strong>Select the extension directory</strong>
                <span class="fl">Folder:</span>
                <span class="pf"><span class="typed2" style="--n:{folderChars}">{folder}</span></span>
                <span class="ok">Select Folder</span>
              </div>
            </div>
          {:else}
            <div class="link">
              <div class="node"><svg viewBox="0 0 24 24" width="30" height="30" fill="none" stroke="currentColor" stroke-width="1.5"><circle cx="12" cy="12" r="9" /><path d="M3 12h18M12 3c3 3.2 3 14.8 0 18M12 3c-3 3.2-3 14.8 0 18" /></svg><span>{name}</span></div>
              <span class="wire" class:ok={connected}></span>
              <div class="node"><svg viewBox="0 0 34 34" width="32" height="32"><path d="M8 9v9M13 9v12M18 9v9M23 9v12" stroke="var(--signal)" stroke-width="2.4" stroke-linecap="round" /><path d="M7 25h20" stroke="var(--fg)" stroke-width="2.4" stroke-linecap="round" /></svg><span>AmpSync</span></div>
            </div>
          {/if}
        {/key}
      </div>

      <div class="copy">
        {#if step === 0}
          <h4>Open the extensions page</h4>
          <p>AmpSync opens it in {name} for you. If nothing opens, copy this address into {name}'s address bar.</p>
          <div class="row2">
            <div class="field" aria-label="Extensions page address">{url}</div>
            <button type="button" class="btn" onclick={() => copy(url, "url")}>{copied === "url" ? "Copied ✓" : "Copy"}</button>
          </div>
          {#if hint}<p class="hint warn" role="status">{hint}</p>{/if}
        {:else if step === 1}
          <h4>Turn on Developer mode</h4>
          <p>{developerModeWhere(browser)} It only lets {name} add an extension from a folder on your computer.</p>
          <p class="small">If {name} later suggests turning developer extensions off, close that message with ✕. Turning them off unlinks AmpSync.</p>
        {:else if step === 2}
          <h4>Add the AmpSync extension</h4>
          <p>Click <strong>Load unpacked</strong>. In the folder window, paste this path into the address bar and choose <strong>Select Folder</strong>.</p>
          <div class="row3">
            <div class="field path" title={folder}>{folder || "Preparing the extension…"}</div>
            <button type="button" class="btn" disabled={!folder} onclick={() => copy(folder, "path")}>{copied === "path" ? "Copied ✓" : "Copy path"}</button>
            <button type="button" class="btn ghost" disabled={!folder} onclick={reveal}>{revealLabel(app.info?.os)}</button>
          </div>
          <p class="small">AmpSync unpacked the extension here for you. Need it elsewhere? <button type="button" class="linkbtn" onclick={saveZip}>Save the zip</button>.</p>
        {:else if connected}
          <div aria-live="polite">
            <h4>Linked to {name}</h4>
            <span class="state ok">connected</span>
            <p>From now on, when a sign-in runs out AmpSync quietly takes a fresh one from {name}. You'll only see a Microsoft sign-in if {name} itself asks for one.</p>
          </div>
        {:else}
          <div aria-live="polite">
            <h4>Connecting…</h4>
            <span class="state wait">waiting for the extension</span>
            <p>This takes a few seconds once the extension is added. AmpSync connects by itself, so there's nothing to click.</p>
          </div>
        {/if}
      </div>

      <div class="foot">
        <button type="button" class="btn ghost notnow" onclick={close}>Not now, use the sign-in window</button>
        <span class="btns">
          {#if step > 0}<button type="button" class="btn ghost" onclick={() => go(step - 1)}>Back</button>{/if}
          <button type="button" class="btn primary" bind:this={primary} disabled={!primaryEnabled(step, connected)} onclick={next}>{step === 0 && hint ? "I've opened it" : primaryLabel(step, browser, connected)}</button>
        </span>
      </div>
    </section>
  </div>
</div>

<style>
  .scrim { position: fixed; inset: 0; z-index: 70; background: rgba(6, 8, 11, .66); display: grid; place-items: center; padding: 16px; overflow: auto; }
  .guide {
    width: min(780px, 100%); background: var(--raise); border: 1px solid var(--line); border-top: 2px solid var(--signal);
    border-radius: 4px; box-shadow: 0 12px 32px rgba(0, 0, 0, .4); display: grid; grid-template-columns: 220px minmax(0, 1fr);
  }
  aside { background: var(--panel); border-right: 1px solid var(--line); padding: 18px 14px 14px; display: flex; flex-direction: column; gap: 16px; }
  .pick { display: grid; gap: 8px; }
  .seg { display: flex; border: 1px solid var(--line); border-radius: 4px; overflow: hidden; }
  .seg button { flex: 1; background: none; border: 0; padding: 4px 12px; font-size: 12px; color: var(--muted); cursor: pointer; }
  .seg button[aria-pressed="true"] { background: var(--bg); color: var(--fg); }
  .steps { list-style: none; margin: 0; padding: 0; display: grid; gap: 2px; }
  .stp {
    display: flex; gap: 10px; align-items: flex-start; width: 100%; text-align: left; background: none; border: 0;
    border-left: 2px solid transparent; padding: 8px 10px; color: var(--muted); font-size: 13px; cursor: pointer; border-radius: 0 4px 4px 0;
  }
  .stp .n { flex: none; width: 20px; height: 20px; border-radius: 50%; border: 1px solid var(--line); display: grid; place-items: center; font: 500 11px var(--f-mono); }
  .stp.now { color: var(--fg); background: var(--bg); border-left-color: var(--signal); }
  .stp.now .n { border-color: var(--signal); color: var(--signal); }
  .stp.done .n { background: var(--ok); border-color: var(--ok); color: var(--bg); }
  .lock { margin-top: auto; display: flex; gap: 8px; align-items: flex-start; font-size: 11.5px; color: var(--muted); line-height: 1.45; }
  .lock svg { flex: none; margin-top: 2px; }

  section { padding: 18px 20px 16px; display: flex; flex-direction: column; gap: 14px; min-width: 0; }
  .head { display: flex; justify-content: space-between; align-items: flex-start; gap: 12px; }
  h3 { margin: 0 0 4px; font: 600 19px/1.2 var(--f-display); }
  .lede { margin: 0; font-size: 13px; color: var(--muted); }
  .close { padding: 6px; }

  .copy { display: grid; gap: 8px; min-height: 124px; align-content: start; }
  .copy h4 { margin: 0; font: 600 15px var(--f-display); }
  .copy p { margin: 0; font-size: 13px; color: var(--muted); }
  .copy p.small { font-size: 12px; }
  .copy p.warn { color: var(--crit); }
  .copy strong { color: var(--fg); font-weight: 600; }
  .row2 { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 8px; max-width: 360px; }
  .row3 { display: grid; grid-template-columns: minmax(0, 1fr) auto auto; gap: 8px; }
  .field { padding: 8px 12px; }
  .field.path { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .linkbtn { background: none; border: 0; padding: 0; font: inherit; color: var(--signal); cursor: pointer; text-decoration: underline; text-underline-offset: 3px; }
  .linkbtn:hover { filter: brightness(1.15); }
  .state { font: 12px var(--f-mono); display: inline-flex; align-items: center; gap: 6px; }
  .state::before { content: ""; width: 6px; height: 6px; background: currentColor; }
  .state.ok { color: var(--ok); }
  .state.wait { color: var(--info); animation: breathe 1.6s ease-in-out infinite; }
  .foot { display: flex; justify-content: space-between; align-items: center; gap: 10px; flex-wrap: wrap; border-top: 1px solid var(--line); padding-top: 12px; margin-top: auto; }
  .notnow { border: 0; padding-left: 0; color: var(--muted); }
  .btns { display: flex; gap: 8px; }

  /* The illustration. The browser window is drawn the way a browser looks, in light colours in both themes. */
  .ill { position: relative; height: 236px; background: var(--bg); border: 1px solid var(--line); border-radius: 4px; overflow: hidden; }
  .bw {
    position: absolute; left: 22px; right: 22px; top: 18px; bottom: 0; background: #f3f4f6; border-radius: 6px 6px 0 0; color: #1f2329;
    font: 12px/1.4 "Segoe UI", var(--f-body); overflow: hidden; box-shadow: 0 6px 18px rgba(0, 0, 0, .35);
  }
  .tabs { height: 26px; background: #dfe2e6; display: flex; align-items: flex-end; padding: 0 8px; gap: 6px; }
  .tab { background: #f3f4f6; border-radius: 6px 6px 0 0; padding: 4px 12px; font-size: 11px; color: #30353c; }
  .addr { display: flex; align-items: center; gap: 8px; padding: 6px 10px; border-bottom: 1px solid #d6d9dd; }
  .addr .nav { color: #6b7280; }
  .addr .box { flex: 1; background: #fff; border: 1px solid #c9cdd2; border-radius: 14px; padding: 3px 12px; font: 12px var(--f-mono); color: #1f2329; display: flex; align-items: center; }
  .typed { display: inline-block; overflow: hidden; white-space: nowrap; width: 0; animation: type 1.6s steps(var(--n)) .4s forwards; }
  .caret { display: inline-block; width: 1px; height: 13px; background: #1f2329; margin-left: 1px; animation: blink 1s step-end infinite; }
  .skel { padding: 18px; display: grid; gap: 10px; }
  .ph { height: 8px; border-radius: 4px; background: #dfe2e6; }
  .pg { display: grid; grid-template-columns: 150px 1fr; height: 100%; }
  .side { background: #eceef1; border-right: 1px solid #d6d9dd; padding: 10px 8px; display: flex; flex-direction: column; gap: 4px; font-size: 11.5px; color: #30353c; }
  .side-h { padding: 2px 8px 6px; font-size: 13px; }
  .side .it { padding: 4px 8px; border-radius: 4px; }
  .side .it.sel { background: #dce6f5; color: #0f3a75; }
  .devrow { position: relative; display: flex; align-items: center; justify-content: space-between; padding: 6px 8px; border-top: 1px solid #d6d9dd; margin-top: auto; margin-bottom: 64px; font-weight: 600; }
  .chrome-pg { position: relative; height: 100%; }
  .chrome-h { display: flex; justify-content: space-between; align-items: center; padding: 10px 14px; border-bottom: 1px solid #d6d9dd; }
  .chrome-h strong { font-size: 14px; }
  .dev { display: flex; gap: 8px; align-items: center; font-weight: 600; position: relative; }
  .dev2 { margin-left: auto; display: flex; gap: 6px; align-items: center; font-size: 11px; }
  .tg { position: relative; width: 30px; height: 16px; border-radius: 8px; background: #9aa1ab; flex: none; animation: tgon 3.4s infinite; }
  .tg::after { content: ""; position: absolute; top: 2px; left: 2px; width: 12px; height: 12px; border-radius: 50%; background: #fff; animation: knob 3.4s infinite; }
  .tg.on { animation: none; background: #2f6fd6; }
  .tg.on::after { animation: none; left: 16px; }
  .ring { position: absolute; border-radius: 6px; pointer-events: none; animation: ring 1.4s ease-out infinite; }
  .cursor { position: absolute; width: 18px; height: 18px; pointer-events: none; filter: drop-shadow(0 1px 1px rgba(0, 0, 0, .4)); }
  .cur-a { animation: cura 3.4s infinite; }
  .cur-b { animation: curb 3.2s infinite; }
  .main-pg { padding: 12px 14px; display: flex; flex-direction: column; gap: 10px; }
  .tb { display: flex; gap: 6px; align-items: center; }
  .tb .b { border: 1px solid #c9cdd2; background: #fff; border-radius: 4px; padding: 3px 9px; font-size: 11px; color: #1f2329; }
  .tb .b.hl { border-color: #2f6fd6; color: #0f3a75; position: relative; }
  .card { border: 1px solid #d6d9dd; background: #fff; border-radius: 6px; padding: 8px 10px; display: flex; gap: 10px; align-items: center; }
  .card .icon { width: 24px; height: 24px; border-radius: 4px; background: #dfe2e6; }
  .card .lines { flex: 1; display: grid; gap: 5px; }
  .picker {
    position: absolute; right: 30px; bottom: 14px; width: 300px; background: #fff; border: 1px solid #b9bec5; border-radius: 6px;
    box-shadow: 0 10px 24px rgba(0, 0, 0, .3); padding: 8px 10px; display: grid; gap: 6px; color: #1f2329;
    font: 11.5px "Segoe UI", var(--f-body); opacity: 0; animation: rise .4s ease-out 1.2s forwards;
  }
  .picker .fl { color: #4b5563; }
  .picker .pf { border: 1px solid #2f6fd6; border-radius: 3px; padding: 3px 6px; font: 11px var(--f-mono); white-space: nowrap; overflow: hidden; }
  .picker .typed2 { display: inline-block; overflow: hidden; white-space: nowrap; width: 0; vertical-align: bottom; animation: type2 .9s steps(var(--n)) 1.8s forwards; }
  .picker .ok { justify-self: end; background: #2f6fd6; color: #fff; border-radius: 3px; padding: 3px 12px; }
  .link { position: absolute; inset: 0; display: flex; align-items: center; justify-content: center; }
  .node {
    width: 96px; height: 96px; border-radius: 8px; border: 1px solid var(--line); background: var(--raise); display: grid; place-items: center;
    align-content: center; gap: 6px; font-weight: 500; font-size: 12px; color: var(--fg);
  }
  .wire { width: 170px; height: 2px; background: repeating-linear-gradient(90deg, var(--signal) 0 8px, transparent 8px 16px); animation: flow .7s linear infinite; }
  .wire.ok { background: var(--ok); animation: none; }

  @keyframes type { to { width: calc(var(--n) * 1ch); } }
  @keyframes type2 { to { width: calc(var(--n) * 1ch); } }
  @keyframes blink { 50% { opacity: 0; } }
  @keyframes knob { 0%, 40% { left: 2px; } 55%, 100% { left: 16px; } }
  @keyframes tgon { 0%, 40% { background: #9aa1ab; } 55%, 100% { background: #2f6fd6; } }
  @keyframes ring { 0% { box-shadow: 0 0 0 0 rgba(240, 164, 58, .75); } 100% { box-shadow: 0 0 0 12px rgba(240, 164, 58, 0); } }
  @keyframes cura { 0% { transform: translate(260px, -120px); } 35%, 100% { transform: translate(0, 0); } }
  @keyframes curb { 0% { transform: translate(220px, 90px); } 30%, 100% { transform: translate(0, 0); } }
  @keyframes rise { from { opacity: 0; transform: translateY(10px); } to { opacity: 1; transform: none; } }
  @keyframes flow { to { background-position: 16px 0; } }
  @keyframes breathe { 50% { opacity: .45; } }

  /* Still pictures at their end state: the system preference, or the app's Reduced motion setting. */
  .calm * { animation: none !important; }
  .calm .typed, .calm .picker .typed2 { width: calc(var(--n) * 1ch); }
  .calm .picker { opacity: 1; }
  .calm .tg { background: #2f6fd6; }
  .calm .tg::after { left: 16px; }
  .calm .ring { box-shadow: 0 0 0 4px rgba(240, 164, 58, .45); }
  @media (prefers-reduced-motion: reduce) {
    * { animation: none !important; }
    .typed, .picker .typed2 { width: calc(var(--n) * 1ch); }
    .picker { opacity: 1; }
    .tg { background: #2f6fd6; }
    .tg::after { left: 16px; }
    .ring { box-shadow: 0 0 0 4px rgba(240, 164, 58, .45); }
  }
  @media (max-width: 720px) {
    .guide { grid-template-columns: minmax(0, 1fr); }
    aside { border-right: 0; border-bottom: 1px solid var(--line); }
    .lock { margin-top: 0; }
    .row3 { grid-template-columns: minmax(0, 1fr); }
  }
</style>
