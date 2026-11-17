<script lang="ts">
  import { api, toCmdError } from "../core/api";
  import { app } from "../core/store.svelte";
  import { ago } from "../core/format";
  import type { SessionInfo } from "../core/types";
  import { renewalsText } from "../core/linkguide";
  import SignInForm from "../parts/SignInForm.svelte";
  import { METHOD_NAMES, browserName, defaultFromValue, defaultValue, isLinked, signInDefaults } from "../core/signin";

  let newLink = $state("");
  let confirmOut = $state<string | null>(null);
  let refreshing = $state(false);

  let confirmUnlink = $state(false);
  let testing = $state(false);

  const link = $derived(app.link);
  const linked = $derived(link?.enabled === true);
  const name = $derived(browserName(link?.browser));
  const hasLinked = $derived(linked && app.sessions.some((x) => isLinked(x.method)));
  const defaults = $derived(signInDefaults(link?.browser));

  function statusText(s: SessionInfo): string {
    switch (s.status) {
      case "active": return isLinked(s.method) ? "active · renews automatically" : `active · ${ago(s.captured_at)}`;
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

  async function testNow() {
    testing = true;
    try {
      const r = await api.linkTest();
      app.toast(r.message);
    } catch (e) {
      app.toast(toCmdError(e).message);
    } finally {
      testing = false;
    }
  }

  async function unlink() {
    if (!confirmUnlink) {
      confirmUnlink = true;
      return;
    }
    confirmUnlink = false;
    try {
      await api.linkUnlink();
      app.toast("Browser link removed. You can also remove the extension from your browser.");
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

{#if linked && link}
  <div class="panel linked">
    <div class="panel-h">
      <span class="ttl">
        <span class="label">Browser link</span>
        <span class="state {link.connected ? 's-ok' : 's-wait'}">{link.connected ? `connected to ${name}` : `not connected${link.browser ? ` to ${name}` : ""}`}</span>
      </span>
      <span class="btns">
        <button type="button" class="btn ghost small" disabled={testing || !link.connected} onclick={testNow}>{testing ? "Testing..." : "Test now"}</button>
        <button type="button" class="btn ghost small" onclick={() => app.openGuide()}>Set-up guide</button>
        <button type="button" class="btn ghost small" onclick={unlink} onblur={() => (confirmUnlink = false)}>{confirmUnlink ? "Click again to unlink" : "Unlink"}</button>
      </span>
    </div>
    <p class="blurb">
      {#if link.connected}
        AmpSync takes your SharePoint sign-in from {name} whenever it runs out, so downloads don't stop. If {name} itself is signed out, you'll get one Microsoft prompt in {name}, not here.
      {:else}
        The AmpSync extension isn't connected right now. Open {link.browser ? name : "your browser"} and AmpSync reconnects by itself, or open the set-up guide to add the extension again.
      {/if}
    </p>
    <div class="facts">
      <div><span class="label">Last renewed</span><span class="meta fg">{link.last_renewed ? `${ago(link.last_renewed)} · automatic` : "not yet"}</span></div>
      <div><span class="label">Renewals this week</span><span class="meta fg">{renewalsText(link.renewals_7d, link.prompts_7d)}</span></div>
      <div><span class="label">Extension</span><span class="meta fg">{link.ext_version ? `AmpSync Browser Link ${link.ext_version}` : "AmpSync Browser Link"}</span></div>
    </div>
  </div>
{:else}
  <div class="promo">
    <div class="txt">
      <h2>Tired of signing in?</h2>
      <p>Link AmpSync to Edge or Chrome once. AmpSync then reuses the sign-in your browser already keeps fresh, and the sign-in windows stop for good.</p>
    </div>
    <span class="cta"><button type="button" class="btn primary" onclick={() => app.openGuide()}>Set up browser link</button><span class="meta">about 2 minutes</span></span>
  </div>
{/if}

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
              <td>{METHOD_NAMES[s.method]}</td>
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
      {#each defaults as o (o.value)}<option value={o.value}>{o.label}</option>{/each}
    </select>
    <span class="meta">{hasLinked ? "others are only used if the link stops working" : linked ? "" : "set up the browser link to stop these prompts"}</span>
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
  With the browser link on, the AmpSync extension in your browser shares only your SharePoint sign-in with AmpSync on this computer. It can't see passwords, other sites or your browsing history. Unlink at any time, here or by removing the extension. Without it, AmpSync keeps its own browser profile and never reads your Edge, Chrome or Teams sign-ins.
</p>

<style>
  .linked { border-color: color-mix(in srgb, var(--ok) 55%, var(--line)); }
  .ttl, .btns { display: flex; gap: 10px; align-items: center; flex-wrap: wrap; }
  .btns { gap: 8px; }
  .blurb { margin: 0; padding: 12px 14px; font-size: 13px; color: var(--muted); }
  .facts { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); border-top: 1px solid var(--line); }
  .facts > div { padding: 10px 14px; border-right: 1px solid var(--line); display: grid; gap: 4px; min-width: 0; }
  .facts > div:last-child { border-right: 0; }
  .meta.fg { color: var(--fg); overflow-wrap: anywhere; }
  .promo {
    display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 16px; align-items: center; padding: 18px 20px;
    border: 1px dashed var(--signal); border-radius: 4px; background: var(--bg);
  }
  .promo .txt { display: grid; gap: 4px; }
  .promo h2 { margin: 0; font: 600 20px var(--f-display); }
  .promo p { margin: 0; color: var(--muted); font-size: 13px; max-width: 62ch; }
  .cta { display: grid; gap: 6px; justify-items: end; }
  @media (max-width: 700px) {
    .promo { grid-template-columns: minmax(0, 1fr); }
    .cta { justify-items: start; }
    .facts { grid-template-columns: minmax(0, 1fr); }
    .facts > div { border-right: 0; border-bottom: 1px solid var(--line); }
  }
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
