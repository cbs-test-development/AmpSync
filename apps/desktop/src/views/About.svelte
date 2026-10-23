<script lang="ts">
  import { api } from "../core/api";
  import { app } from "../core/store.svelte";
  import { initials } from "../core/format";

  const palette = ["#f0a43a", "#6ea8d8", "#62b88a", "#c58bd9", "#e0655a", "#8a93a2"];
  function color(login: string): string {
    let h = 0;
    for (const c of login) h = (h * 31 + c.charCodeAt(0)) >>> 0;
    return palette[h % palette.length];
  }

  const info = $derived(app.info);
  const people = $derived(app.contributors.items);
  const repo = $derived(app.contributors.repo || info?.repo || "");
</script>

<div class="about">
  <div class="panel">
    <div class="panel-h"><span class="label">AmpSync</span><span class="meta">v{info?.version ?? ""}</span></div>
    <div class="blurb">
      <p>Downloads large SharePoint folders file by file, checks every file, and resumes after any interruption. No zip files, no OneDrive client.</p>
    </div>
    {#if app.update}
      <div class="upd">
        <span>Version {app.update.version} is available.</span>
        <button type="button" class="btn" onclick={() => app.update && api.openExternal(app.update.url)}>See what's new</button>
      </div>
    {/if}
    <dl class="spec">
      <dt>License</dt><dd>MIT</dd>
      {#if repo}
        <dt>Source</dt>
        <dd><button type="button" class="linkbtn" onclick={() => api.openExternal(`https://github.com/${repo}`)}>github.com/{repo}</button></dd>
      {/if}
      <dt>Build</dt><dd>{info?.build ?? ""} · {info?.target ?? ""}</dd>
      <dt>Telemetry</dt><dd>none</dd>
    </dl>
  </div>
  <div class="panel">
    <div class="panel-h"><span class="label">Contributors</span><span class="meta">from GitHub at build time</span></div>
    {#if people.length > 0}
      <div class="people">
        {#each people as p (p.login)}
          <div class="person">
            {#if p.avatar}
              <img class="avatar" src={p.avatar} alt="" width="40" height="40" />
            {:else}
              <span class="avatar" style:background={color(p.login)}>{initials(p.name || p.login)}</span>
            {/if}
            <div class="who">
              <div class="nm">{p.name || p.login}</div>
              <div class="un">@{p.login}</div>
              <div class="cm">{p.contributions} {p.contributions === 1 ? "commit" : "commits"}</div>
            </div>
          </div>
        {/each}
      </div>
    {:else}
      <p class="none">The contributor list is added in release builds. This build was made without one.</p>
    {/if}
  </div>
</div>

<style>
  .about { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1.4fr); gap: 18px; }
  .blurb { padding: 14px 14px 4px; }
  .blurb p { margin: 0 0 10px; max-width: 62ch; }
  .upd { display: flex; gap: 12px; align-items: center; justify-content: space-between; flex-wrap: wrap; margin: 0 14px 10px; padding: 8px 12px; border: 1px solid var(--info); border-radius: 4px; font-size: 13px; }
  .spec { display: grid; grid-template-columns: auto minmax(0, 1fr); gap: 6px 16px; font: 12px var(--f-mono); padding: 14px; margin: 0; }
  .spec dt { color: var(--muted); }
  .spec dd { margin: 0; overflow-wrap: anywhere; }
  .linkbtn { background: none; border: 0; padding: 0; color: var(--signal); font: inherit; cursor: pointer; text-decoration: underline; text-underline-offset: 3px; text-align: left; }
  .people { display: grid; grid-template-columns: repeat(auto-fill, minmax(200px, 1fr)); }
  .person { display: grid; grid-template-columns: 40px minmax(0, 1fr); gap: 12px; align-items: center; padding: 12px 14px; border-bottom: 1px solid var(--line); border-right: 1px solid var(--line); }
  .avatar { width: 40px; height: 40px; border-radius: 4px; display: grid; place-items: center; font: 600 14px var(--f-display); color: var(--signal-ink); object-fit: cover; }
  .who { min-width: 0; }
  .nm { font-weight: 600; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .un { font: 12px var(--f-mono); color: var(--muted); overflow: hidden; text-overflow: ellipsis; }
  .cm { font: 11px var(--f-mono); color: var(--muted); }
  .none { margin: 0; padding: 18px 14px; color: var(--muted); font-size: 13px; }
  @media (max-width: 920px) { .about { grid-template-columns: minmax(0, 1fr); } }
</style>
