<script lang="ts">
  // "How do you want to sign in?" (D26). The backend asks when a link needs a sign-in and no way
  // is remembered; the answer, or Cancel, goes back through `choose_sign_in`.
  import { onMount } from "svelte";
  import { app } from "../core/store.svelte";
  import { api } from "../core/api";

  let remember = $state(true);
  let first: HTMLButtonElement;

  function answer(method: "webview" | "edge" | "chrome" | null) {
    app.signInChoose = null;
    void api.chooseSignIn(method, remember);
  }

  onMount(() => {
    first?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") answer(null);
    };
    addEventListener("keydown", onKey);
    return () => removeEventListener("keydown", onKey);
  });
</script>

<div class="scrim">
  <div class="dlg" role="dialog" aria-modal="true" aria-labelledby="choose-title">
    <h3 id="choose-title">How do you want to sign in?</h3>
    <p class="host"><span class="label">Site</span> {app.signInChoose}</p>
    <div class="opts">
      <button type="button" class="opt" bind:this={first} onclick={() => answer("webview")}>
        <strong>Via AmpSync</strong>
        <span>Recommended for personal devices</span>
      </button>
      <div class="opt">
        <strong>Via Edge or Chrome</strong>
        <span>Recommended for work devices</span>
        <span class="btns">
          <button type="button" class="btn" onclick={() => answer("edge")}>Use Edge</button>
          <button type="button" class="btn" onclick={() => answer("chrome")}>Use Chrome</button>
        </span>
      </div>
    </div>
    <label class="remember"><input type="checkbox" bind:checked={remember} /> Remember my choice</label>
    <div class="row">
      <button type="button" class="btn ghost" onclick={() => answer(null)}>Cancel</button>
    </div>
  </div>
</div>

<style>
  .scrim { position: fixed; inset: 0; z-index: 70; background: rgba(6, 8, 11, .62); display: grid; place-items: center; padding: 16px; }
  .dlg {
    width: min(460px, 100%); background: var(--raise); border: 1px solid var(--line); border-top: 2px solid var(--signal);
    border-radius: 4px; padding: 16px 18px 14px; box-shadow: 0 12px 32px rgba(0, 0, 0, .35);
  }
  h3 { margin: 0 0 6px; font: 600 17px/1.25 var(--f-display); }
  .host { margin: 0 0 12px; font-size: 13px; color: var(--muted); display: flex; gap: 8px; align-items: baseline; word-break: break-all; }
  .opts { display: grid; gap: 8px; margin-bottom: 12px; }
  .opt {
    display: grid; gap: 2px; justify-items: start; text-align: left; font: inherit; color: var(--fg);
    background: var(--panel); border: 1px solid var(--line); border-radius: 4px; padding: 10px 12px;
  }
  button.opt { cursor: pointer; }
  button.opt:hover, button.opt:focus-visible { border-color: var(--signal); }
  .opt strong { font-weight: 600; font-size: 14px; }
  .opt span { font-size: 12px; color: var(--muted); }
  .opt .btns { display: flex; gap: 8px; margin-top: 8px; }
  .remember { display: flex; gap: 8px; align-items: center; font-size: 13px; margin-bottom: 14px; cursor: pointer; }
  .remember input { accent-color: var(--signal); }
  .row { display: flex; justify-content: flex-end; }
</style>
