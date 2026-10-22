<script lang="ts">
  import { onMount } from "svelte";
  import { app } from "./core/store.svelte";
  import { api } from "./core/api";
  import type { ViewId } from "./core/types";
  import Transfers from "./views/Transfers.svelte";
  import NewDownload from "./views/NewDownload.svelte";
  import SignIns from "./views/SignIns.svelte";
  import SettingsView from "./views/Settings.svelte";
  import About from "./views/About.svelte";
  import Tour from "./parts/Tour.svelte";
  import ClosePrompt from "./parts/ClosePrompt.svelte";

  const titles: Record<ViewId, string> = {
    transfers: "Transfers",
    new: "New download",
    signins: "Sign-ins",
    settings: "Settings",
    about: "About",
  };

  onMount(() => {
    app.init().catch((e) => console.error("start-up failed", e));
  });

  const pending = $derived(app.queue.filter((q) => ["queued", "preparing", "running", "paused", "needs_sign_in"].includes(q.status)).length);

  const chip = $derived.by(() => {
    const s = app.sessions;
    if (s.length === 0) return { bad: false, text: "not signed in" };
    const bad = s.find((x) => x.status === "expired");
    if (bad) return { bad: true, text: `${bad.host} · sign-in expired` };
    if (s.length === 1) return { bad: false, text: `${s[0].host} · signed in` };
    return { bad: false, text: `${s.length} sites · signed in` };
  });
</script>

<div class="app">
  <nav class="rail" aria-label="Main">
    <svg class="mark" viewBox="0 0 34 34" role="img" aria-label="AmpSync">
      <rect x="1" y="1" width="32" height="32" rx="4" fill="none" stroke="var(--line)" stroke-width="1.5" />
      <path d="M8 9v9M13 9v12M18 9v9M23 9v12" stroke="var(--signal)" stroke-width="2.4" stroke-linecap="round" />
      <path d="M7 25h20" stroke="var(--fg)" stroke-width="2.4" stroke-linecap="round" />
    </svg>
    <button type="button" data-go="transfers" aria-current={app.view === "transfers" ? "page" : undefined} title="Transfers" aria-label="Transfers" onclick={() => app.go("transfers")}>
      <svg viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.6"><path d="M10 3v10M6 9l4 4 4-4M4 16h12" /></svg>
      {#if pending > 0}<span class="badge">{pending}</span>{/if}
    </button>
    <button type="button" data-go="new" aria-current={app.view === "new" ? "page" : undefined} title="New download" aria-label="New download" onclick={() => app.go("new")}>
      <svg viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.6"><path d="M10 4v12M4 10h12" /></svg>
    </button>
    <button type="button" data-go="signins" aria-current={app.view === "signins" ? "page" : undefined} title="Sign-ins" aria-label="Sign-ins" onclick={() => app.go("signins")}>
      <svg viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.6"><circle cx="7.5" cy="10" r="3.5" /><path d="M11 10h6M15 10v3M17 10v2" /></svg>
      {#if app.expired.length > 0}<span class="badge crit">!</span>{/if}
    </button>
    <span class="spacer"></span>
    <button type="button" data-go="settings" aria-current={app.view === "settings" ? "page" : undefined} title="Settings" aria-label="Settings" onclick={() => app.go("settings")}>
      <svg viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.6"><path d="M4 6h8M15 6h1M4 14h2M9 14h7" /><circle cx="13.5" cy="6" r="1.6" /><circle cx="7.5" cy="14" r="1.6" /></svg>
    </button>
    <button type="button" data-go="about" aria-current={app.view === "about" ? "page" : undefined} title="About and contributors" aria-label="About and contributors" onclick={() => app.go("about")}>
      <svg viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.6"><circle cx="10" cy="10" r="7" /><path d="M10 9v5M10 6.2v.1" /></svg>
      {#if app.update}<span class="badge dot-badge" aria-label="Update available"></span>{/if}
    </button>
  </nav>

  <main>
    <header class="topbar">
      <h1>{titles[app.view]}</h1>
      <div class="top-right">
        {#if app.update}
          <button type="button" class="update-link" onclick={() => app.update && api.openExternal(app.update.url)}>
            v{app.update.version} available
          </button>
        {/if}
        <button type="button" class="session-chip" onclick={() => app.go("signins")} title="Open Sign-ins">
          <span class="dot" class:bad={chip.bad}></span><span>{chip.text}</span>
        </button>
      </div>
    </header>

    <div class="scroll">
      <section class="view" hidden={app.view !== "transfers"} data-view="transfers"><Transfers /></section>
      <section class="view" hidden={app.view !== "new"} data-view="new"><NewDownload /></section>
      <section class="view" hidden={app.view !== "signins"} data-view="signins"><SignIns /></section>
      <section class="view" hidden={app.view !== "settings"} data-view="settings"><SettingsView /></section>
      <section class="view" hidden={app.view !== "about"} data-view="about"><About /></section>
    </div>
  </main>
</div>

{#if app.toastText}
  <div class="toast" role="status">{app.toastText}</div>
{/if}

{#if app.tourOpen}<Tour />{/if}
{#if app.closePrompt}<ClosePrompt />{/if}

<style>
  .app {
    height: 100vh; display: grid; grid-template-columns: 64px minmax(0, 1fr); background: var(--panel);
  }
  .rail {
    background: var(--bg); border-right: 1px solid var(--line); display: flex; flex-direction: column;
    align-items: center; padding-block: 14px; gap: 6px;
  }
  .mark { width: 34px; height: 34px; margin-bottom: 14px; }
  .rail button {
    width: 46px; height: 46px; display: grid; place-items: center; background: none; border: 0;
    border-radius: 4px; color: var(--muted); cursor: pointer; position: relative;
  }
  .rail button:hover { color: var(--fg); background: var(--raise); }
  .rail button[aria-current="page"] { color: var(--signal); background: var(--raise); }
  .rail button[aria-current="page"]::before {
    content: ""; position: absolute; left: -9px; top: 10px; bottom: 10px; width: 2px; background: var(--signal);
  }
  .rail svg:not(.mark) { width: 20px; height: 20px; }
  .spacer { flex: 1; }
  .badge {
    position: absolute; top: 6px; right: 6px; min-width: 15px; height: 15px; border-radius: 8px; padding: 0 4px;
    background: var(--signal); color: var(--signal-ink); font: 600 10px/15px var(--f-mono);
  }
  .badge.crit { background: var(--crit); color: #fff; }
  .badge.dot-badge { min-width: 9px; height: 9px; padding: 0; top: 9px; right: 9px; }

  main { min-width: 0; min-height: 0; display: flex; flex-direction: column; }
  .scroll { flex: 1; min-height: 0; overflow-y: auto; }
  .topbar {
    display: flex; align-items: center; justify-content: space-between; gap: 16px; flex-wrap: wrap;
    padding: 14px 22px; border-bottom: 1px solid var(--line);
  }
  .topbar h1 { margin: 0; font: 600 18px/1.2 var(--f-display); letter-spacing: .01em; text-wrap: balance; }
  .top-right { display: flex; gap: 12px; align-items: center; flex-wrap: wrap; }
  .session-chip {
    display: inline-flex; align-items: center; gap: 8px; font: 12px var(--f-mono); color: var(--muted);
    border: 1px solid var(--line); border-radius: 3px; padding: 4px 10px; background: var(--raise); cursor: pointer;
  }
  .session-chip:hover { border-color: var(--muted); }
  .update-link {
    background: none; border: 0; color: var(--signal); font: 500 12px var(--f-mono); cursor: pointer;
    text-decoration: underline; text-underline-offset: 3px; padding: 2px;
  }
  .dot { width: 7px; height: 7px; border-radius: 50%; background: var(--ok); flex: none; }
  .dot.bad { background: var(--crit); }
  .view[hidden] { display: none !important; }
  .toast {
    position: fixed; left: 50%; bottom: 24px; transform: translateX(-50%); z-index: 70; max-width: min(560px, calc(100vw - 32px));
    background: var(--fg); color: var(--bg); padding: 9px 16px; border-radius: 4px; font-size: 13px; font-weight: 500;
  }
  @media (max-width: 920px) {
    .app { grid-template-columns: 64px minmax(0, 1fr); }
  }
  @media (max-width: 600px) {
    .app { grid-template-columns: minmax(0, 1fr); grid-template-rows: auto minmax(0, 1fr); }
    .rail { flex-direction: row; justify-content: space-around; padding: 6px; border-right: 0; border-bottom: 1px solid var(--line); }
    .mark, .spacer { display: none; }
    .rail button[aria-current="page"]::before { left: 10px; right: 10px; top: auto; bottom: -6px; width: auto; height: 2px; }
    .topbar { padding: 12px 14px; }
  }
</style>
