<script lang="ts">
  import { open as openDialog } from "@tauri-apps/plugin-dialog";
  import { app, type Theme } from "../core/store.svelte";
  import { api, toCmdError } from "../core/api";
  import type { CloseBehavior } from "../core/types";

  const MB = 1024 * 1024;
  const caps = [
    { label: "No limit", value: 0 },
    { label: "50 MB/s", value: 50 * MB },
    { label: "20 MB/s", value: 20 * MB },
    { label: "5 MB/s", value: 5 * MB },
    { label: "1 MB/s", value: 1 * MB },
  ];

  const s = $derived(app.settings);
  const capValue = $derived(s?.bandwidth_limit ?? 0);
  const capKnown = $derived(caps.some((c) => c.value === capValue));

  let destDraft = $state("");
  let destDirty = false;
  $effect(() => {
    if (!destDirty) destDraft = s?.default_dest ?? "";
  });

  async function commitDest() {
    destDirty = false;
    const v = destDraft.trim();
    await app.saveSettings({ default_dest: v === "" ? null : v });
  }

  async function browse() {
    try {
      const picked = await openDialog({ directory: true, multiple: false, defaultPath: destDraft || undefined });
      if (typeof picked === "string") {
        destDraft = picked;
        await commitDest();
      }
    } catch (e) {
      app.toast(toCmdError(e).message);
    }
  }

  function step(delta: number) {
    if (!s) return;
    const next = Math.min(16, Math.max(1, s.parallel_files + delta));
    void app.saveSettings({ parallel_files: next });
  }

  const themes: { id: Theme; label: string }[] = [
    { id: "system", label: "System" },
    { id: "dark", label: "Dark" },
    { id: "light", label: "Light" },
  ];

  let confirmRemove = $state(false);
  let removing = $state(false);
  let cancelBtn = $state<HTMLButtonElement>();

  $effect(() => {
    if (!confirmRemove) return;
    cancelBtn?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !removing) confirmRemove = false;
    };
    addEventListener("keydown", onKey);
    return () => removeEventListener("keydown", onKey);
  });

  async function removeAll() {
    removing = true;
    try {
      // On success AmpSync closes, cleans up and opens again by itself.
      await api.purgeEverything();
    } catch (e) {
      removing = false;
      confirmRemove = false;
      app.toast(toCmdError(e).message);
    }
  }

  const isWindows = $derived(app.info?.os === "windows");
</script>

{#if s}
  <div class="panel">
    <div class="panel-h"><span class="label">Downloads</span><span class="meta">saved automatically</span></div>
    <div class="form-grid">
      <div>
        <label class="label" for="s-dest">Default folder</label>
        <div class="paste">
          <input class="field" id="s-dest" bind:value={destDraft} oninput={() => (destDirty = true)} onblur={commitDest} onkeydown={(e) => e.key === "Enter" && commitDest()} spellcheck="false" autocomplete="off" placeholder="Ask me each time" />
          <button type="button" class="btn" onclick={browse}>Browse</button>
        </div>
      </div>
      <div>
        <span class="label" id="par-label">Files at the same time</span>
        <div class="stepper" role="group" aria-labelledby="par-label">
          <button type="button" aria-label="Fewer" onclick={() => step(-1)} disabled={s.parallel_files <= 1}>−</button>
          <output aria-live="polite">{s.parallel_files}</output>
          <button type="button" aria-label="More" onclick={() => step(1)} disabled={s.parallel_files >= 16}>+</button>
        </div>
        <p class="hint">Applies to the next download that starts.</p>
      </div>
      <div>
        <label class="label" for="s-cap">Speed limit</label>
        <select class="field" id="s-cap" value={String(capValue)} onchange={(e) => app.saveSettings({ bandwidth_limit: Number(e.currentTarget.value) || null })}>
          {#each caps as c}<option value={String(c.value)}>{c.label}</option>{/each}
          {#if !capKnown}<option value={String(capValue)}>{(capValue / MB).toFixed(1)} MB/s</option>{/if}
        </select>
      </div>
      <div>
        <span class="label" id="close-label">When the window closes</span>
        <div class="radio-row" style="padding-top: 6px" role="radiogroup" aria-labelledby="close-label">
          {#each [["ask", "Ask"], ["tray", "Keep going in tray"], ["pause", "Pause"]] as [v, text]}
            <label><input type="radio" name="close" value={v} checked={s.close_behavior === v} onchange={() => app.saveSettings({ close_behavior: v as CloseBehavior })} /> {text}</label>
          {/each}
        </div>
      </div>
    </div>
  </div>

  <div class="panel">
    <div class="toggle">
      <div><strong>Use my Windows work account</strong><p>{isWindows ? "Signs in with the account you use to log in to Windows when your company allows it, so there's often no prompt. Takes effect the next time AmpSync starts." : "Windows only."}</p></div>
      <span class="switch"><input type="checkbox" aria-label="Use my Windows work account" checked={s.use_windows_account} disabled={!isWindows} onchange={(e) => app.saveSettings({ use_windows_account: e.currentTarget.checked })} /><i></i></span>
    </div>
    <div class="toggle">
      <div><strong>Remember passwords in the sign-in window</strong><p>Off by default. Only affects AmpSync's own sign-in window, never your browsers.</p></div>
      <span class="switch"><input type="checkbox" aria-label="Remember passwords in the sign-in window" checked={s.save_passwords} onchange={(e) => app.saveSettings({ save_passwords: e.currentTarget.checked })} /><i></i></span>
    </div>
    <div class="toggle">
      <div><strong>Check for new versions</strong><p>Looks at GitHub Releases once a day. Nothing else is sent, and nothing is installed without you.</p></div>
      <span class="switch"><input type="checkbox" aria-label="Check for new versions" checked={s.check_updates} onchange={(e) => app.saveSettings({ check_updates: e.currentTarget.checked })} /><i></i></span>
    </div>
    <div class="toggle">
      <div><strong>Walkthrough</strong><p>The short tour shown the first time AmpSync opens.</p></div>
      <button type="button" class="btn" id="replay" onclick={() => app.startTour()}>Show again</button>
    </div>
  </div>

  <div class="panel">
    <div class="toggle">
      <div><strong>Appearance</strong><p>Follow the system, or pick a theme.</p></div>
      <div class="theme-switch" role="group" aria-label="Theme">
        {#each themes as t}
          <button type="button" aria-pressed={app.theme === t.id} onclick={() => app.setTheme(t.id)}>{t.label}</button>
        {/each}
      </div>
    </div>
  </div>

  <div class="panel danger-zone">
    <div class="toggle">
      <div><strong>Remove all AmpSync data</strong><p>Signs you out everywhere and deletes your settings, saved sign-ins and download history from this computer. Your downloaded files are not touched. AmpSync then restarts as if it was just installed.</p></div>
      <button type="button" class="btn danger" id="remove-all" onclick={() => (confirmRemove = true)}>Remove all data</button>
    </div>
  </div>

  {#if confirmRemove}
    <div class="scrim">
      <div class="dlg" role="alertdialog" aria-modal="true" aria-labelledby="rm-title" aria-describedby="rm-body">
        <h3 id="rm-title">Remove all AmpSync data?</h3>
        <p id="rm-body">This signs you out of every SharePoint site and deletes your settings, queue and download history. AmpSync closes and opens again from scratch. Files you already downloaded stay where they are. This cannot be undone.</p>
        <div class="row">
          <button type="button" class="btn ghost" bind:this={cancelBtn} disabled={removing} onclick={() => (confirmRemove = false)}>Cancel</button>
          <button type="button" class="btn danger" disabled={removing} onclick={removeAll}>{removing ? "Removing…" : "Remove everything"}</button>
        </div>
      </div>
    </div>
  {/if}
{/if}

<style>
  .danger-zone { border-color: var(--crit); }
  .scrim { position: fixed; inset: 0; z-index: 60; background: rgba(6, 8, 11, .62); display: grid; place-items: center; padding: 16px; }
  .dlg {
    width: min(460px, 100%); background: var(--raise); border: 1px solid var(--line); border-top: 2px solid var(--crit);
    border-radius: 4px; padding: 16px 18px 14px; box-shadow: 0 12px 32px rgba(0, 0, 0, .35);
  }
  .dlg h3 { margin: 0 0 6px; font: 600 17px/1.25 var(--f-display); }
  .dlg p { margin: 0 0 14px; font-size: 13px; color: var(--muted); }
  .dlg .row { display: flex; justify-content: space-between; align-items: center; gap: 10px; flex-wrap: wrap; }
  .toggle { display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 12px 14px; border-bottom: 1px solid var(--line); }
  .toggle:last-child { border-bottom: 0; }
  .toggle p { margin: 2px 0 0; font-size: 12px; color: var(--muted); max-width: 70ch; }
  .switch { position: relative; width: 36px; height: 20px; flex: none; }
  .switch input { position: absolute; opacity: 0; inset: 0; margin: 0; cursor: pointer; }
  .switch input:disabled { cursor: default; }
  .switch i { position: absolute; inset: 0; background: var(--grid); border: 1px solid var(--line); border-radius: 10px; pointer-events: none; }
  .switch i::after { content: ""; position: absolute; top: 2px; left: 2px; width: 14px; height: 14px; border-radius: 50%; background: var(--muted); transition: transform .15s; }
  .switch input:checked + i { background: var(--signal); border-color: var(--signal); }
  .switch input:checked + i::after { transform: translateX(16px); background: var(--signal-ink); }
  .switch input:focus-visible + i { outline: 2px solid var(--signal); outline-offset: 2px; }
  .switch input:disabled + i { opacity: .5; }
  .stepper { display: flex; align-items: center; border: 1px solid var(--line); border-radius: 4px; width: max-content; }
  .stepper button { background: none; border: 0; width: 32px; height: 32px; cursor: pointer; color: var(--muted); font-size: 16px; }
  .stepper button:hover:not(:disabled) { color: var(--fg); }
  .stepper button:disabled { opacity: .4; cursor: default; }
  .stepper output { min-width: 34px; text-align: center; font: 500 14px var(--f-mono); }
  .theme-switch { display: flex; border: 1px solid var(--line); border-radius: 4px; overflow: hidden; }
  .theme-switch button { background: none; border: 0; padding: 4px 12px; cursor: pointer; font-size: 12px; color: var(--muted); }
  .theme-switch button[aria-pressed="true"] { background: var(--raise); color: var(--fg); }
  @media (prefers-reduced-motion: reduce) {
    .switch i::after { transition: none; }
  }
</style>
