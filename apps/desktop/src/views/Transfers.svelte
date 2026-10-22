<script lang="ts">
  import { app } from "../core/store.svelte";
  import { api, toCmdError } from "../core/api";
  import { duration, num, rate, size, sizeParts } from "../core/format";
  import type { QueueItem } from "../core/types";
  import Chart from "../parts/Chart.svelte";

  const MB = 1024 * 1024;

  const job = $derived(app.active);
  const live = $derived(job ? app.live[job.id] : undefined);

  const doneBytes = $derived(live?.bytes_done ?? 0);
  const totalBytes = $derived(live?.bytes_total || job?.est_bytes || 0);
  const pct = $derived(totalBytes > 0 ? Math.min(100, (doneBytes / totalBytes) * 100) : 0);
  const rateNow = $derived(live && !live.paused && !live.waiting_sign_in ? live.rate_bps : 0);
  const etaSeconds = $derived(rateNow > 0 ? (totalBytes - doneBytes) / rateNow : NaN);
  const peak = $derived(live ? Math.max(0, ...live.hist.map((h) => h.v)) : 0);
  const filesDone = $derived((live?.verified ?? 0) + (live?.skipped ?? 0));
  const filesTotal = $derived(live?.files_total || job?.est_files || 0);
  const lanesNow = $derived(live?.lanes ?? []);

  const headLabel = $derived.by(() => {
    switch (job?.status) {
      case "preparing": return "Getting ready";
      case "paused": return "Paused";
      case "needs_sign_in": return "Waiting for sign-in";
      default: return "Downloading";
    }
  });

  const laneStatus = $derived(
    job?.status === "needs_sign_in" ? "waiting for sign-in" : job?.status === "paused" ? "paused" : "",
  );

  function stateClass(s: string): string {
    return { running: "s-run", ok: "s-ok", skipped: "s-skip", retrying: "s-wait", failed: "s-bad" }[s] ?? "s-q";
  }
  function stateText(s: string): string {
    return { running: "downloading", ok: "saved", skipped: "up to date", retrying: "retrying", failed: "failed" }[s] ?? s;
  }

  async function act(fn: () => Promise<unknown>) {
    try {
      await fn();
    } catch (e) {
      app.toast(toCmdError(e).message);
    }
  }

  async function pauseOrResume() {
    if (!job) return;
    if (job.status === "paused") {
      await act(() => api.resume(job.id));
      app.toast("Resumed");
    } else {
      await act(() => api.pause(job.id));
      app.toast("Paused. Finished files are kept.");
    }
  }

  let confirmCancel = $state<number | null>(null);

  async function cancelJob(id: number) {
    if (confirmCancel !== id) {
      confirmCancel = id;
      return;
    }
    confirmCancel = null;
    await act(() => api.cancel(id));
    app.toast("Stopped. Files already saved are kept.");
  }

  async function signInAgain() {
    if (!job) return;
    try {
      await api.signIn(job.spec.source, "webview");
      app.toast("Signed in. The download continues.");
    } catch (e) {
      const err = toCmdError(e);
      if (err.kind === "cancelled") app.toast("Sign-in window closed. Try again when you're ready.");
      else app.toast(err.message);
    }
  }

  function statusText(q: QueueItem): string {
    switch (q.status) {
      case "queued": return "waiting";
      case "preparing": return "getting ready";
      case "running": return "downloading";
      case "paused": return "paused";
      case "needs_sign_in": return "needs sign-in";
      case "done": return q.summary && q.summary.failed > 0 ? `${q.summary.failed} failed` : "finished";
      case "failed": return "stopped";
      case "cancelled": return "cancelled";
    }
  }
  function statusClass(q: QueueItem): string {
    switch (q.status) {
      case "done": return q.summary && q.summary.failed > 0 ? "s-bad" : "s-ok";
      case "running": return "s-run";
      case "failed": return "s-bad";
      case "needs_sign_in": return "s-bad";
      case "preparing": return "s-wait";
      default: return "s-q";
    }
  }
</script>

{#if job && job.status === "needs_sign_in"}
  <div class="alert" role="alert">
    <p>
      <strong>Sign-in expired.</strong>
      {num(filesDone)} {filesDone === 1 ? "file is" : "files are"} already saved. Sign in to {job.host} again and the download picks up where it stopped.
    </p>
    <button type="button" class="btn primary" onclick={signInAgain} disabled={app.signInOpen !== null}>
      {app.signInOpen ? "Sign-in window is open" : "Sign in again"}
    </button>
  </div>
{/if}

{#if job}
  <div class="job-head">
    <div>
      <span class="label">{headLabel}</span>
      <h2>{job.title}</h2>
      <div class="path">{job.path_label}</div>
    </div>
    <div class="actions">
      {#if job.status !== "needs_sign_in"}
        <button type="button" class="btn" onclick={pauseOrResume} disabled={job.status === "preparing"}>
          {#if job.status === "paused"}
            <svg viewBox="0 0 14 14" fill="currentColor" aria-hidden="true"><path d="M3 2l9 5-9 5z" /></svg><span>Resume</span>
          {:else}
            <svg viewBox="0 0 14 14" fill="currentColor" aria-hidden="true"><rect x="3" y="2" width="3" height="10" /><rect x="8" y="2" width="3" height="10" /></svg><span>Pause</span>
          {/if}
        </button>
      {/if}
      <button type="button" class="btn ghost" onclick={() => cancelJob(job.id)} onblur={() => (confirmCancel = null)}>
        {confirmCancel === job.id ? "Click again to stop" : "Stop"}
      </button>
    </div>
  </div>

  <div data-tour="progress" class="readout">
    <div>
      <span class="label">Transferred</span>
      <div class="v">{sizeParts(doneBytes)[0]} <small>{sizeParts(doneBytes)[1]} / {size(totalBytes)}</small></div>
      <div class="sub">{pct.toFixed(1)}%</div>
    </div>
    <div>
      <span class="label">Speed</span>
      <div class="v">{rate(rateNow)[0]} <small>{rate(rateNow)[1]}</small></div>
      <div class="sub">{app.settings?.parallel_files ?? 6} at a time</div>
    </div>
    <div>
      <span class="label">Time left</span>
      <div class="v">{isFinite(etaSeconds) ? duration(etaSeconds) : "-"}</div>
      <div class="sub">{isFinite(etaSeconds) ? "at current speed" : laneStatus || "working it out"}</div>
    </div>
    <div>
      <span class="label">Files</span>
      <div class="v">{num(filesDone)} <small>/ {num(filesTotal)}</small></div>
      <div class="sub">
        <span class="s-ok">{num(live?.verified ?? 0)} checked</span> · {num(live?.skipped ?? 0)} already saved ·
        <span class:s-bad={(live?.failed ?? 0) > 0}>{num(live?.failed ?? 0)} failed</span>
      </div>
    </div>
  </div>
  <div class="bar ticks" role="progressbar" aria-label="Overall progress" aria-valuemin="0" aria-valuemax="100" aria-valuenow={Math.round(pct)}><i style:width="{pct.toFixed(2)}%"></i></div>

  <div class="grid-2">
    <div class="panel">
      <div class="panel-h"><span class="label">Speed · last 2 min</span><span class="meta">peak {num(peak / MB, 1)} MB/s</span></div>
      {#if live}<Chart hist={live.hist} />{/if}
    </div>
    <div class="panel col lanes-panel">
      <div class="panel-h"><span class="label">Parallel lanes</span><span class="meta">{lanesNow.length} at a time</span></div>
      <div class="lanes body">
        {#each lanesNow as lane, i}
          {@const waiting = laneStatus !== ""}
          <div class="lane" class:waiting>
            <span class="id">L{i + 1}</span>
            <div class="lane-body">
              <div class="file" title={lane.rel_path ?? ""}>{lane.rel_path ?? "idle"}</div>
              <div class="rng">
                <span>{waiting ? laneStatus : lane.rel_path ? `at ${num(lane.offset / MB, 0)} MB` : ""}</span>
                <span>{lane.rel_path ? size(lane.size) : ""}</span>
              </div>
              <div class="bar"><i style:width="{lane.rel_path && lane.size > 0 ? ((lane.offset / lane.size) * 100).toFixed(1) : 0}%"></i></div>
            </div>
          </div>
        {/each}
      </div>
    </div>
  </div>
{:else}
  <div class="empty" data-tour="progress">
    <h2>{app.queue.length > 0 ? "Nothing is downloading right now" : "No downloads yet"}</h2>
    <p>
      {#if app.queue.length > 0}
        Downloads wait here until you resume them, or start by themselves once they reach the front of the queue.
      {:else}
        Paste a SharePoint link and pick the folders you need. AmpSync saves them file by file, checks each one, and carries on after any interruption.
      {/if}
    </p>
    <button type="button" class="btn primary" onclick={() => app.go("new")}>Paste a link</button>
  </div>
{/if}

<div class="grid-2 fill-row">
  <div class="panel col">
    <div class="panel-h"><span class="label">Files</span><span class="meta">most recent first</span></div>
    {#if live && live.rows.length > 0}
      <div class="table-wrap body">
        <table>
          <thead><tr><th class="label">Name</th><th class="label num">Size</th><th class="label">State</th><th class="label">Checked by</th></tr></thead>
          <tbody>
            {#each live.rows as r (r.rel_path)}
              <tr>
                <td class="name" title={r.rel_path}>{r.rel_path}</td>
                <td class="num">{r.size === null ? "-" : size(r.size)}</td>
                <td><span class="state {stateClass(r.state)}">{stateText(r.state)}</span></td>
                <td class="check" title={r.check}>{r.check}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {:else}
      <p class="panel-empty body">Files appear here as they are saved.</p>
    {/if}
  </div>

  <div class="panel col" data-tour="queue">
    <div class="panel-h"><span class="label">Queue</span><span class="meta">runs in order</span></div>
    <div class="queue body">
      {#if job}
        <div class="qitem active"><span class="t">{job.title}</span><span class="sz">{size(job.est_bytes)}</span><span class="p">{job.path_label}</span></div>
      {/if}
      {#each app.waiting as q (q.id)}
        <div class="qitem">
          <span class="t">{q.title}</span><span class="sz">{size(q.est_bytes)}</span>
          <span class="p">{q.path_label}</span>
          <span class="q-actions">
            <span class="state {statusClass(q)}">{statusText(q)}</span>
            {#if q.status === "paused"}
              <button type="button" class="link" onclick={() => act(() => api.resume(q.id))}>Resume</button>
            {:else}
              <button type="button" class="link" onclick={() => act(() => api.pause(q.id))}>Hold</button>
            {/if}
            <button type="button" class="link" onclick={() => act(() => api.cancel(q.id))}>Remove</button>
          </span>
        </div>
      {/each}
      {#if !job && app.waiting.length === 0}
        <p class="panel-empty">Nothing queued. Downloads you add run one after another.</p>
      {/if}
      {#if app.finished.length > 0}
        <div class="q-sep"><span class="label">Finished</span><button type="button" class="link" onclick={() => act(() => api.clearFinished())}>Clear</button></div>
        {#each app.finished as q (q.id)}
          <div class="qitem">
            <span class="t">{q.title}</span><span class="state {statusClass(q)}">{statusText(q)}</span>
            <span class="p">
              {#if q.summary}
                {num(q.summary.downloaded)} saved · {num(q.summary.skipped)} already there · {size(q.summary.bytes)}
              {:else if q.error}
                {q.error}
              {:else}
                {q.path_label}
              {/if}
            </span>
            <span class="q-actions">
              <button type="button" class="link" onclick={() => act(() => api.retry(q.id))}>{q.status === "done" && !q.summary?.failed ? "Check again" : "Try again"}</button>
            </span>
          </div>
        {/each}
      {/if}
    </div>
  </div>
</div>

<style>
  .job-head { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 14px 24px; align-items: end; }
  .job-head h2 { margin: 2px 0 4px; font: 600 22px/1.2 var(--f-display); text-wrap: balance; }
  .path { font: 12px var(--f-mono); color: var(--muted); overflow-wrap: anywhere; }
  .actions { display: flex; gap: 8px; flex-wrap: wrap; }

  .readout { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); border: 1px solid var(--line); border-radius: 4px; background: var(--bg); }
  .readout > div { padding: 12px 16px; border-right: 1px solid var(--line); min-width: 0; }
  .readout > div:last-child { border-right: 0; }
  .v { font: 500 22px/1.25 var(--f-mono); font-variant-numeric: tabular-nums; margin-top: 6px; white-space: nowrap; }
  .v small { font-size: 13px; color: var(--muted); }
  .sub { font: 12px var(--f-mono); color: var(--muted); }

  .lanes { display: grid; align-content: start; }
  /* The lanes panel takes the chart's height and scrolls when there are more lanes than fit. */
  .lanes-panel { contain: size; }
  .fill-row { grid-template-rows: minmax(0, 1fr); }
  .lane { display: grid; grid-template-columns: 34px minmax(0, 1fr); gap: 10px; align-items: center; padding: 9px 14px; border-bottom: 1px solid var(--line); }
  .lane:last-child { border-bottom: 0; }
  .lane .id { font: 500 11px var(--f-mono); color: var(--signal); }
  .lane-body { min-width: 0; }
  .lane .file { font: 12px var(--f-mono); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .lane .rng { font: 11px var(--f-mono); color: var(--muted); display: flex; justify-content: space-between; gap: 8px; margin-top: 2px; min-height: 16px; }
  .lane .bar { height: 4px; margin-top: 5px; }
  .lane.waiting .id { color: var(--info); }
  .lane.waiting .bar > i { background: var(--info); }

  .check { font: 11px var(--f-mono); color: var(--muted); max-width: 220px; overflow: hidden; text-overflow: ellipsis; }
  .panel-empty { margin: 0; padding: 18px 14px; color: var(--muted); font-size: 13px; }

  .queue { display: grid; align-content: start; }
  .qitem { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 4px 12px; padding: 10px 14px; border-bottom: 1px solid var(--line); }
  .qitem:last-child { border-bottom: 0; }
  .qitem .t { font-weight: 500; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .qitem .p { font: 11px var(--f-mono); color: var(--muted); grid-column: 1 / -1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .qitem .sz { font: 12px var(--f-mono); color: var(--muted); }
  .qitem.active .t::before { content: "▸ "; color: var(--signal); }
  .q-actions { grid-column: 1 / -1; display: flex; gap: 14px; align-items: center; }
  .q-sep { display: flex; justify-content: space-between; align-items: center; padding: 10px 14px 6px; border-bottom: 1px solid var(--line); }
  .link { background: none; border: 0; padding: 2px; color: var(--muted); cursor: pointer; font-size: 12px; text-decoration: underline; text-underline-offset: 3px; }
  .link:hover { color: var(--fg); }

  @media (max-height: 720px) {
    .v { font-size: 19px; margin-top: 3px; }
    .readout > div { padding: 8px 14px; }
    .job-head h2 { font-size: 19px; margin: 0 0 2px; }
  }
  @media (max-width: 800px) {
    .lanes-panel { contain: none; max-height: 260px; }
    .fill-row { grid-template-rows: repeat(2, minmax(0, 1fr)); }
  }
  @media (max-width: 760px) {
    .readout { grid-template-columns: repeat(2, minmax(0, 1fr)); }
    .readout > div:nth-child(2) { border-right: 0; }
    .readout > div:nth-child(-n + 2) { border-bottom: 1px solid var(--line); }
  }
  @media (max-width: 600px) {
    .job-head { grid-template-columns: minmax(0, 1fr); }
    .v { font-size: 18px; }
  }
</style>
