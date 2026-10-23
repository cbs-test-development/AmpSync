<script lang="ts">
  import { api, toCmdError } from "../core/api";
  import { app } from "../core/store.svelte";

  let {
    link,
    compact = false,
    ondone,
  }: { link: string; compact?: boolean; ondone?: () => void } = $props();

  let busy = $state<string | null>(null);
  let message = $state("");
  let pasteOpen = $state(false);
  let cookie = $state("");

  async function run(method: "webview" | "edge" | "chrome" | "paste") {
    if (!link.trim()) {
      message = "Enter the SharePoint address first.";
      return;
    }
    busy = method;
    message = "";
    try {
      await api.signIn(link, method, method === "paste" ? cookie : undefined);
      cookie = "";
      pasteOpen = false;
      app.toast("Signed in.");
      await app.refreshSessions();
      ondone?.();
    } catch (e) {
      const err = toCmdError(e);
      message =
        err.kind === "cancelled"
          ? "The sign-in window was closed before sign-in finished."
          : err.message;
    } finally {
      busy = null;
    }
  }

  async function stop() {
    await api.cancelSignIn();
  }
</script>

<div class="methods" class:compact>
  <div class="method">
    <span class="n">1</span>
    <h3>AmpSync window</h3>
    <p>A sign-in window opens inside the app. Works for most accounts, including multi-factor sign-in.</p>
    {#if busy === "webview"}
      <button type="button" class="btn" onclick={stop}>Cancel sign-in</button>
    {:else}
      <button type="button" class="btn primary" disabled={busy !== null} onclick={() => run("webview")}>Open sign-in window</button>
    {/if}
  </div>
  <div class="method">
    <span class="n">2</span>
    <h3>Edge or Chrome</h3>
    <p>If your company blocks the app window, AmpSync opens your browser with a separate, temporary profile. Your normal profile is never touched.</p>
    <div class="row">
      <button type="button" class="btn" disabled={busy !== null} onclick={() => run("edge")}>{busy === "edge" ? "Waiting for Edge..." : "Use Edge"}</button>
      <button type="button" class="btn" disabled={busy !== null} onclick={() => run("chrome")}>{busy === "chrome" ? "Waiting for Chrome..." : "Use Chrome"}</button>
    </div>
  </div>
  <div class="method">
    <span class="n">3</span>
    <h3>Paste a session</h3>
    <p>Last resort for computers without a screen. Copy the cookie header from your browser's developer tools.</p>
    <button type="button" class="btn" disabled={busy !== null} aria-expanded={pasteOpen} onclick={() => (pasteOpen = !pasteOpen)}>Paste cookie header</button>
  </div>
</div>

{#if pasteOpen}
  <div class="paste-box">
    <label class="label" for="cookie-header">Cookie header</label>
    <textarea id="cookie-header" class="field" bind:value={cookie} spellcheck="false" autocomplete="off" placeholder="FedAuth=...; rtFa=..."></textarea>
    <p class="hint">It is stored only in your system keychain, never written to a file.</p>
    <button type="button" class="btn primary" disabled={busy !== null || !cookie.trim()} onclick={() => run("paste")}>Save session</button>
  </div>
{/if}

{#if message}<p class="msg" role="alert">{message}</p>{/if}

<style>
  .methods { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); }
  .method { padding: 14px; border-right: 1px solid var(--line); display: grid; gap: 8px; align-content: start; justify-items: start; }
  .method:last-child { border-right: 0; }
  .n { font: 500 11px var(--f-mono); color: var(--signal); }
  h3 { margin: 0; font: 600 15px var(--f-display); }
  p { margin: 0; font-size: 13px; color: var(--muted); }
  .row { display: flex; gap: 8px; flex-wrap: wrap; }
  .paste-box { display: grid; gap: 8px; padding: 14px; border-top: 1px solid var(--line); justify-items: start; }
  .paste-box .field { width: 100%; }
  .msg { padding: 10px 14px; color: var(--crit); border-top: 1px solid var(--line); font-size: 13px; }
  @media (max-width: 920px) {
    .methods { grid-template-columns: minmax(0, 1fr); }
    .method { border-right: 0; border-bottom: 1px solid var(--line); }
  }
</style>
