<script lang="ts">
  import { api, toCmdError } from "../core/api";
  import { app } from "../core/store.svelte";
  import { ago } from "../core/format";
  import type { SessionInfo } from "../core/types";
  import SignInForm from "../parts/SignInForm.svelte";
  import { SIGN_IN_DEFAULTS, defaultFromValue, defaultValue } from "../core/signin";

  let newLink = $state("");
  let confirmOut = $state<string | null>(null);
  let refreshing = $state(false);

  const methodName: Record<SessionInfo["method"], string> = {
    webview: "AmpSync window",
    edge: "Edge (separate profile)",
    chrome: "Chrome (separate profile)",
    paste: "Pasted session",
  };

  function statusText(s: SessionInfo): string {
    switch (s.status) {
      case "active": return `active · ${ago(s.captured_at)}`;
      case "expired": return "expired";
      case "checking": return "checking...";
      default: return `signed in ${ago(s.captured_at)}`;
    }
  }
  function statusClass(s: SessionInfo): string {
    return s.status === "active" ? "s-ok" : s.status === "expired" ? "s-bad" : s.status === "checking" ? "s-wait" : "s-skip";
  }

  async function signInAgain(host: string) {
    try {
      // "auto": the remembered way, or the question if none is remembered.
      await api.signIn(`https://${host}/`, "auto");
      app.toast("Signed in.");
    } catch (e) {
      const err = toCmdError(e);
      if (err.kind !== "cancelled") app.toast(err.message);
    }
  }

  async function signOut(host: string) {
    if (confirmOut !== host) {
      confirmOut = host;
      return;
    }
    confirmOut = null;
    try {
      await api.signOut(host);
      await app.refreshSessions();
      app.toast(`Signed out of ${host}.`);
    } catch (e) {
      app.toast(toCmdError(e).message);
    }
  }

  async function refresh() {
    refreshing = true;
    try {
      await api.checkSessions();
    } catch (e) {
      app.toast(toCmdError(e).message);
    } finally {
      await app.refreshSessions();
      refreshing = false;
    }
  }
</script>

<div class="panel">
  <div class="panel-h">
    <span class="label">Signed-in SharePoint sites</span>
    <span class="head-right">
      <span class="meta">stored in {app.info?.credential_store ?? "the system keyring"}</span>
      <button type="button" class="btn ghost small" onclick={refresh} disabled={refreshing || app.sessions.length === 0}>{refreshing ? "Checking..." : "Check now"}</button>
    </span>
  </div>
  {#if app.sessions.length === 0}
    <p class="empty-row">You're not signed in to any site yet. Sign in below, or just paste a link in New download and AmpSync asks when it needs to.</p>
  {:else}
    <div class="table-wrap">
      <table>
        <thead><tr><th class="label">Address</th><th class="label">Signed in with</th><th class="label">Status</th><th></th></tr></thead>
        <tbody>
          {#each app.sessions as s (s.host)}
            <tr>
              <td class="name">{s.host}</td>
              <td>{methodName[s.method]}</td>
              <td><span class="state {statusClass(s)}">{statusText(s)}</span></td>
              <td class="num actions">
                {#if s.status === "expired"}
                  <button type="button" class="btn" disabled={app.signInOpen !== null} onclick={() => signInAgain(s.host)}>Sign in</button>
                {/if}
                <button type="button" class="btn ghost" onclick={() => signOut(s.host)} onblur={() => (confirmOut = null)}>
                  {confirmOut === s.host ? "Click again to sign out" : "Sign out"}
                </button>
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</div>

<div class="panel">
  <div class="panel-h"><span class="label">When AmpSync needs you to sign in</span></div>
  <div class="dflt">
    <label class="label" for="si-default">Sign in with</label>
    <select class="field" id="si-default" value={defaultValue(app.settings?.sign_in_with)} disabled={app.settings === null} onchange={(e) => app.saveSettings({ sign_in_with: defaultFromValue(e.currentTarget.value) })}>
      {#each SIGN_IN_DEFAULTS as o (o.value)}<option value={o.value}>{o.label}</option>{/each}
    </select>
  </div>
</div>

<div class="panel">
  <div class="panel-h"><span class="label">Sign in to a site</span><span class="meta">tried in this order</span></div>
  <div class="addr">
    <label class="label" for="si-link">SharePoint address or link</label>
    <input class="field" id="si-link" bind:value={newLink} spellcheck="false" autocomplete="off" placeholder="https://your-company.sharepoint.com" />
  </div>
  <SignInForm link={newLink} />
</div>

<p class="privacy">
  AmpSync keeps its own browser profile and never reads or uses your Edge, Chrome or Teams sign-ins. Signing out removes the stored session and clears AmpSync's own profile.
</p>

<style>
  .head-right { display: flex; gap: 12px; align-items: center; flex-wrap: wrap; }
  .small { padding: 3px 10px; font-size: 12px; }
  .actions { white-space: nowrap; }
  .actions .btn + .btn { margin-left: 8px; }
  .empty-row { margin: 0; padding: 18px 14px; color: var(--muted); font-size: 13px; }
  .addr { display: grid; gap: 6px; padding: 14px; border-bottom: 1px solid var(--line); }
  .dflt { display: flex; gap: 12px; align-items: center; flex-wrap: wrap; padding: 12px 14px; font-size: 13px; }
  .dflt select { min-width: 180px; }
  .privacy { margin: 0; font-size: 12px; color: var(--muted); max-width: 80ch; }
</style>
