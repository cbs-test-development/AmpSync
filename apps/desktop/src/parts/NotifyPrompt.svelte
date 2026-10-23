<script lang="ts">
  // The one-time question about finish notifications (D23). It appears in the Transfers view once
  // a download exists, is answered with a button, is saved to settings and never comes back; the
  // answer can be changed under Settings.
  import { app } from "../core/store.svelte";

  const asking = $derived(app.ready && app.settings !== null && app.settings.notify_on_finish == null && app.queue.length > 0);

  async function answer(yes: boolean) {
    await app.saveSettings({ notify_on_finish: yes });
    app.toast(yes ? "Notifications are on. Turn them off any time in Settings." : "No notifications. You can turn them on in Settings.");
  }
</script>

{#if asking}
  <div class="ask" role="group" aria-labelledby="ask-title">
    <p>
      <strong id="ask-title">Get a notification when downloads finish?</strong>
      <span>AmpSync tells you when a download finishes, stops with an error, or needs you to sign in, and only while its window is in the background.</span>
    </p>
    <div class="acts">
      <button type="button" class="btn primary" onclick={() => answer(true)}>Yes</button>
      <button type="button" class="btn ghost" onclick={() => answer(false)}>No thanks</button>
    </div>
  </div>
{/if}

<style>
  .ask {
    display: flex; gap: 14px; align-items: center; justify-content: space-between; flex-wrap: wrap;
    border: 1px solid var(--line); border-left: 3px solid var(--signal); border-radius: 4px; padding: 10px 14px; background: var(--raise);
  }
  .ask p { margin: 0; font-size: 13px; min-width: 0; flex: 1 1 360px; }
  .ask strong { font-weight: 600; margin-right: 6px; }
  .ask span { color: var(--muted); }
  .acts { display: flex; gap: 8px; flex: none; }
</style>
