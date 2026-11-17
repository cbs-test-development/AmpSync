<script lang="ts">
  // "How do you want to sign in?" (D26). The backend asks when a link needs a sign-in and no way
  // is remembered; the answer, or Cancel, goes back through `choose_sign_in`.
  import { onMount } from "svelte";
  import { app } from "../core/store.svelte";
  import { api } from "../core/api";

  let remember = $state(true);
  let first: HTMLButtonElement;

  const linked = $derived(app.link?.connected === true);

  function answer(method: "webview" | "edge" | "chrome" | "browser_link" | null) {
    app.signInChoose = null;
    void api.chooseSignIn(method, remember);
  }

  /** Not linked yet: the pending question is cancelled (so the backend stops waiting) and the guide opens. */
  function setUp() {
    answer(null);
    app.openGuide();
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
      <div class="opt feat">
        <span class="top"><strong>Through my browser</strong><span class="tag">no sign-in</span></span>
        <span>Uses the SharePoint sign-in Edge or Chrome already keeps fresh. {linked ? "Your browser is linked." : "One-time set-up, about 2 minutes. After that, AmpSync stops asking."}</span>
        <span class="btns">
          <button type="button" class="btn primary" bind:this={first} onclick={() => (linked ? answer("browser_link") : setUp())}>{linked ? "Use my browser" : "Set up browser link"}</button>
        </span>
      </div>
      <button type="button" class="opt" onclick={() => answer("webview")}>
        <strong>Via AmpSync</strong>
        <span>A sign-in window opens inside the app, for this time only</span>
      </button>
      <div class="opt">
        <strong>Via Edge or Chrome, this time only</strong>
        <span>Opens a separate browser window to sign in once</span>
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
  .opt.feat { border-color: var(--signal); background: color-mix(in srgb, var(--signal) 7%, var(--panel)); padding: 12px 14px; }
  .opt .top { display: flex; gap: 8px; align-items: center; }
  .opt .tag {
    font: 600 10px/1 var(--f-display); letter-spacing: .08em; text-transform: uppercase; color: var(--signal-ink);
    background: var(--signal); border-radius: 2px; padding: 3px 5px;
  }
  .opt .btns { display: flex; gap: 8px; margin-top: 8px; }
  .remember { display: flex; gap: 8px; align-items: center; font-size: 13px; margin-bottom: 14px; cursor: pointer; }
  .remember input { accent-color: var(--signal); }
  .row { display: flex; justify-content: flex-end; }
</style>
