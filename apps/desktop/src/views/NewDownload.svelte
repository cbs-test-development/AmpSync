<script lang="ts">
  import { onMount } from "svelte";
  import { open as openDialog } from "@tauri-apps/plugin-dialog";
  import { api, on, toCmdError } from "../core/api";
  import { app } from "../core/store.svelte";
  import { joinPath, num, size } from "../core/format";
  import { TreeModel } from "../core/selection";
  import type { CmdError, PlanTree, Resolved } from "../core/types";
  import FolderTree from "../parts/FolderTree.svelte";
  import SignInForm from "../parts/SignInForm.svelte";

  let link = $state("");
  let phase = $state<"idle" | "working" | "ready">("idle");
  let error = $state<CmdError | null>(null);
  let resolved = $state<Resolved | null>(null);
  let tree = $state.raw<PlanTree | null>(null);
  /** Selection state; plain (not a rune) because it is mutated in place, see `selected`. */
  let model = $state.raw<TreeModel | null>(null);
  let selected = $state({ files: 0, bytes: 0 });
  let listing = $state({ files: 0, bytes: 0 });
  let saveTo = $state("");
  let existing = $state<"skip_unchanged" | "redownload">("skip_unchanged");
  let adding = $state(false);

  $effect(() => {
    // Pre-fill from Settings once, and again when the default changes while the field is empty.
    const d = app.settings?.default_dest;
    if (d && !saveTo) saveTo = d;
  });

  onMount(() => {
    let off: (() => void) | undefined;
    void on<{ files: number; bytes: number }>("listing-progress", (p) => (listing = p)).then((u) => (off = u));
    return () => off?.();
  });

  const sel = $derived(selected);

  function syncSelection() {
    if (model) selected = model.selected;
  }

  async function openFolder() {
    const text = link.trim();
    if (!text) {
      error = { kind: "invalid_link", message: "Paste a SharePoint link first." };
      return;
    }
    phase = "working";
    error = null;
    tree = null;
    model = null;
    resolved = null;
    listing = { files: 0, bytes: 0 };
    try {
      resolved = await api.resolve(text);
      tree = await api.plan(text);
      model = new TreeModel(tree);
      syncSelection();
      phase = "ready";
      await app.refreshSessions();
    } catch (e) {
      error = toCmdError(e);
      phase = "idle";
    }
  }

  async function browse() {
    try {
      const picked = await openDialog({ directory: true, multiple: false, defaultPath: saveTo || undefined });
      if (typeof picked === "string") saveTo = picked;
    } catch (e) {
      app.toast(toCmdError(e).message);
    }
  }

  const needsSignIn = $derived(error && ["auth_expired", "cancelled"].includes(error.kind));

  const errorText = $derived.by(() => {
    if (!error) return "";
    switch (error.kind) {
      case "invalid_link": return error.message.replace(/^invalid link: /, "");
      case "auth_expired":
      case "cancelled": return "You are not signed in to SharePoint yet. Finish signing in, or pick another way below.";
      case "not_found": return "AmpSync could not find that folder. Check the link and that you have access to it.";
      case "network": return "AmpSync could not reach SharePoint. Check your connection and try again.";
      default: return error.message;
    }
  });

  const canAdd = $derived(phase === "ready" && sel.files > 0 && saveTo.trim() !== "" && !adding);

  async function addToQueue() {
    if (!resolved || !tree || !model) return;
    adding = true;
    try {
      const f = model.filters();
      const ahead = app.queue.filter((q) => ["queued", "running", "preparing", "needs_sign_in"].includes(q.status));
      const where = joinPath(saveTo.trim(), resolved.display_name);
      const trail = resolved.server_relative_path.split("/").filter(Boolean).join(" › ");
      await api.enqueue({
        source: link.trim(),
        save_to: saveTo.trim(),
        folder_name: resolved.display_name,
        include: f.include,
        exclude: f.exclude,
        existing,
        title: `${resolved.site_name} · ${resolved.display_name}`,
        path_label: `${resolved.host} › ${trail} → ${where}`,
        est_files: sel.files,
        est_bytes: sel.bytes,
      });
      app.toast(ahead.length > 0 ? `Added to queue. It starts after ${ahead[ahead.length - 1].title}.` : "Added. The download starts now.");
      phase = "idle";
      tree = null;
      model = null;
      resolved = null;
      link = "";
      app.go("transfers");
    } catch (e) {
      app.toast(toCmdError(e).message);
    } finally {
      adding = false;
    }
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Enter") void openFolder();
  }
</script>

<div>
  <label class="label" for="link">SharePoint link</label>
  <div class="paste" style="margin-top: 8px">
    <input
      class="field"
      id="link"
      bind:value={link}
      onkeydown={onKey}
      spellcheck="false"
      autocomplete="off"
      placeholder="https://your-company.sharepoint.com/..."
      disabled={phase === "working"}
    />
    <button type="button" class="btn primary" onclick={openFolder} disabled={phase === "working"}>
      {phase === "working" ? "Opening..." : "Open folder"}
    </button>
  </div>
  <p class="hint">Paste a sharing link, or copy the address bar while you're looking at the folder in SharePoint.</p>
</div>

{#if phase === "working"}
  <div class="alert info" role="status">
    <p>
      {#if app.signInOpen}
        <strong>Sign-in window is open.</strong> Sign in to {app.signInOpen} there; this page continues by itself afterwards.
      {:else if listing.files > 0}
        <strong>Listing folders.</strong> Found {num(listing.files)} files ({size(listing.bytes)}) so far.
      {:else}
        <strong>Opening the folder.</strong> Checking your sign-in and reading the folder list.
      {/if}
    </p>
    {#if app.signInOpen}
      <button type="button" class="btn" onclick={() => api.cancelSignIn()}>Cancel sign-in</button>
    {/if}
  </div>
{/if}

{#if error}
  <div class="alert" role="alert">
    <p><strong>Could not open that folder.</strong> {errorText}</p>
  </div>
  {#if needsSignIn}
    <div class="panel">
      <div class="panel-h"><span class="label">Sign in another way</span><span class="meta">uses the address above</span></div>
      <SignInForm {link} compact ondone={openFolder} />
    </div>
  {/if}
{/if}

<div class="panel" id="picker">
  <div class="panel-h">
    {#if resolved && tree}
      <div class="resolved">
        <span>site <b>{resolved.site_name}</b></span>
        {#if resolved.library}<span>library <b>{resolved.library}</b></span>{/if}
        <span>folder <b>{resolved.display_name}</b></span>
      </div>
      <span class="meta">listed in {(tree.listed_ms / 1000).toFixed(1)} s</span>
    {:else}
      <span class="label">Folders</span>
      <span class="meta">listed with sizes</span>
    {/if}
  </div>
  {#if tree}
    {#if !model || model.n === 0}
      <p class="placeholder">This folder is empty.</p>
    {:else}
      {#key model}
        <FolderTree {model} onchange={syncSelection} />
      {/key}
    {/if}
    <div class="foot-bar">
      <span class="total">{size(sel.bytes)} <small>· {num(sel.files)} {sel.files === 1 ? "file" : "files"} selected</small></span>
      <span class="meta">Untick anything you don't need. Folders open to show their files.</span>
    </div>
  {:else}
    <p class="placeholder">Open a folder above and its subfolders show up here with their sizes, so you can untick what you don't need.</p>
  {/if}
</div>

<div class="panel">
  <div class="panel-h"><span class="label">Where it goes</span></div>
  <div class="form-grid">
    <div>
      <label class="label" for="dest">Save to</label>
      <div class="paste">
        <input class="field" id="dest" bind:value={saveTo} spellcheck="false" autocomplete="off" placeholder="Choose a folder" />
        <button type="button" class="btn" onclick={browse}>Browse</button>
      </div>
      {#if resolved && saveTo}
        <p class="hint">Files go into <span class="mono">{joinPath(saveTo.trim(), resolved.display_name)}</span></p>
      {/if}
    </div>
    <div>
      <span class="label">Files that already exist</span>
      <div class="radio-row" style="padding-top: 6px">
        <label><input type="radio" name="exist" value="skip_unchanged" bind:group={existing} /> Skip if unchanged</label>
        <label><input type="radio" name="exist" value="redownload" bind:group={existing} /> Download again</label>
      </div>
    </div>
  </div>
  <div class="foot-bar">
    <span class="note">Nothing on your computer is deleted.</span>
    <button type="button" class="btn primary" onclick={addToQueue} disabled={!canAdd}>Add to queue</button>
  </div>
</div>

<style>
  .resolved { display: flex; gap: 8px 18px; flex-wrap: wrap; font: 12px var(--f-mono); color: var(--muted); }
  .resolved span b { color: var(--fg); font-weight: 500; }
  .placeholder { margin: 0; padding: 22px 14px; color: var(--muted); font-size: 13px; }
  .total { font: 500 15px var(--f-mono); }
  .total small { color: var(--muted); font-size: 12px; font-weight: 400; }
  .note { font-size: 13px; color: var(--muted); }
</style>
