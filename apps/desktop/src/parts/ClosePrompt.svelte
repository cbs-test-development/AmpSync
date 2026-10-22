<script lang="ts">
  import { onMount } from "svelte";
  import { app } from "../core/store.svelte";
  import { api } from "../core/api";

  let remember = $state(false);
  let first: HTMLButtonElement;

  async function choose(choice: "tray" | "pause") {
    app.closePrompt = false;
    await api.closeDecision(choice, remember);
  }

  function keep() {
    app.closePrompt = false;
  }

  onMount(() => {
    first?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") keep();
    };
    addEventListener("keydown", onKey);
    return () => removeEventListener("keydown", onKey);
  });
</script>

<div class="scrim">
  <div class="dlg" role="dialog" aria-modal="true" aria-labelledby="close-title">
    {#if app.closeActive}
      <h3 id="close-title">A download is still running</h3>
      <p>Keep it going in the system tray, or pause it now. Pausing keeps everything already saved, and the download picks up next time you open AmpSync.</p>
    {:else}
      <h3 id="close-title">Close AmpSync?</h3>
      <p>AmpSync can stay in the system tray so queued downloads start and your sign-in stays fresh, or it can quit completely.</p>
    {/if}
    <label class="remember"><input type="checkbox" bind:checked={remember} /> Remember my choice (change it later in Settings)</label>
    <div class="row">
      <button type="button" class="btn ghost" onclick={keep}>Cancel</button>
      <span class="btns">
        <button type="button" class="btn" onclick={() => choose("pause")}>{app.closeActive ? "Pause and quit" : "Quit"}</button>
        <button type="button" class="btn primary" bind:this={first} onclick={() => choose("tray")}>{app.closeActive ? "Keep going in tray" : "Keep in tray"}</button>
      </span>
    </div>
  </div>
</div>

<style>
  .scrim { position: fixed; inset: 0; z-index: 60; background: rgba(6, 8, 11, .62); display: grid; place-items: center; padding: 16px; }
  .dlg {
    width: min(460px, 100%); background: var(--raise); border: 1px solid var(--line); border-top: 2px solid var(--signal);
    border-radius: 4px; padding: 16px 18px 14px; box-shadow: 0 12px 32px rgba(0, 0, 0, .35);
  }
  h3 { margin: 0 0 6px; font: 600 17px/1.25 var(--f-display); }
  p { margin: 0 0 12px; font-size: 13px; color: var(--muted); }
  .remember { display: flex; gap: 8px; align-items: center; font-size: 13px; margin-bottom: 14px; cursor: pointer; }
  .remember input { accent-color: var(--signal); }
  .row { display: flex; justify-content: space-between; align-items: center; gap: 10px; flex-wrap: wrap; }
  .btns { display: flex; gap: 8px; flex-wrap: wrap; }
</style>
