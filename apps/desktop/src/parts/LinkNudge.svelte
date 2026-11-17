<script lang="ts">
  // The "link my browser" strip under a sign-in-expired alert. Shown while the backend says the
  // nudge is due (`LinkStatus.nudge`); "Not now" keeps it away for 14 days.
  import { app } from "../core/store.svelte";
  import { api, toCmdError } from "../core/api";
  import { nudgeBody, nudgeHeadline } from "../core/linkguide";

  /** The download carries on by itself once the sign-in is renewed (Transfers), as opposed to a folder still to open. */
  let { continues = false }: { continues?: boolean } = $props();

  async function notNow() {
    try {
      await api.linkDismissNudge();
    } catch (e) {
      app.toast(toCmdError(e).message);
    }
  }
</script>

{#if app.link?.nudge}
  <div class="promo" role="region" aria-label="Browser link suggestion">
    <svg viewBox="0 0 24 24" width="22" height="22" fill="none" stroke="var(--signal)" stroke-width="1.6" aria-hidden="true"><path d="M9.5 14.5l5-5" /><path d="M11 6.5l1.8-1.8a4 4 0 0 1 5.6 5.6L16.5 12" /><path d="M13 17.5l-1.8 1.8a4 4 0 0 1-5.6-5.6L7.5 12" /></svg>
    <div class="txt">
      <strong>{nudgeHeadline(app.link.prompts_7d)}</strong>
      <span>{nudgeBody(app.link.browser, continues)}</span>
    </div>
    <span class="btns">
      <button type="button" class="btn ghost" onclick={notNow}>Not now</button>
      <button type="button" class="btn primary" onclick={() => app.openGuide()}>Link my browser</button>
    </span>
  </div>
{/if}

<style>
  .promo {
    display: grid; grid-template-columns: auto minmax(0, 1fr) auto; gap: 14px; align-items: center;
    border: 1px solid var(--line); border-top: 0; border-left: 4px solid var(--signal); border-radius: 0 0 4px 4px;
    padding: 12px 16px; background: var(--panel);
  }
  .txt { display: grid; gap: 2px; }
  .txt strong { font-size: 14px; }
  .txt span { font-size: 12.5px; color: var(--muted); }
  .btns { display: flex; gap: 8px; }
  @media (max-width: 700px) {
    .promo { grid-template-columns: auto minmax(0, 1fr); }
    .btns { grid-column: 1 / -1; justify-content: flex-end; }
  }
</style>
