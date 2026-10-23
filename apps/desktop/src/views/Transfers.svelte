<script lang="ts">
  import { app } from "../core/store.svelte";
  import { api, toCmdError } from "../core/api";
  import { fullText, volumeText } from "../core/diskspace";
  import { inFlight, isRanged, mapSummary, type RangeMap } from "../core/fillmap";
  import { duration, num, rate, size, sizeBrief, sizeParts } from "../core/format";
  import { laneRows, lanesHeader } from "../core/lanes";
  import type { DiskSpace, QueueItem } from "../core/types";
  import Chart from "../parts/Chart.svelte";
  import NotifyPrompt from "../parts/NotifyPrompt.svelte";
  import FillMap from "../parts/FillMap.svelte";

  const MB = 1024 * 1024;

  const job = $derived(app.active);
  const live = $derived(job ? app.live[job.id] : undefined);

  const doneBytes = $derived(live?.bytes_done ?? 0);
  const totalBytes = $derived(live?.bytes_total || job?.est_bytes || 0);
  const pct = $derived(totalBytes > 0 ? Math.min(100, (doneBytes / totalBytes) * 100) : 0);
  const needsSpace = $derived(job?.status === "needs_space");
  // Paused or waiting for sign-in or space: no speed and no time left (the decaying average would
  // otherwise show ~1 B/s and an absurd ETA). Right after a resume the average is still tiny, so the
  // time left stays hidden until the speed is meaningful.
  const stalled = $derived(
    !live || live.paused || live.waiting_sign_in || job?.status === "paused" || job?.status === "needs_sign_in" || needsSpace,
  );
  const rateNow = $derived(stalled ? 0 : live!.rate_bps);
  const MIN_RATE_FOR_ETA = 256 * 1024;
  const MAX_ETA = 30 * 24 * 3600;
  const etaSeconds = $derived.by(() => {
    if (rateNow < MIN_RATE_FOR_ETA) return NaN;
    const s = (totalBytes - doneBytes) / rateNow;
    return s > MAX_ETA ? NaN : s;
  });
  const peak = $derived(live ? Math.max(0, ...live.hist.map((h) => h.v)) : 0);
  const filesDone = $derived((live?.verified ?? 0) + (live?.skipped ?? 0));
  const filesTotal = $derived(live?.files_total || job?.est_files || 0);

  // Lanes (F4): the engine tunes the count by itself unless the user fixed it. Slots are stable;
  // a slot taken away stays on screen until its file is done.
  const auto = $derived(app.settings?.auto_tune ?? false);
  const laneTarget = $derived(live?.lane_target ?? (auto ? 4 : (app.settings?.parallel_files ?? 4)));
  const rows = $derived(laneRows(laneTarget, (live?.lanes ?? []).map((l) => l.rel_path !== null)));
  const shownLanes = $derived((live?.lanes ?? []).slice(0, rows));
  const dense = $derived(rows > 8);
  const laneMin = $derived(dense ? 22 : 32);
  const midMin = $derived(Math.max(190, 38 + Math.ceil(rows / (dense ? 2 : 1)) * laneMin));

  const headLabel = $derived.by(() => {
    switch (job?.status) {
      case "preparing": return "Getting ready";
      case "paused": return "Paused";
      case "needs_sign_in": return "Waiting for sign-in";
      case "needs_space": return "Paused: disk full";
      default: return "Downloading";
    }
  });

  const laneStatus = $derived(
    job?.status === "needs_sign_in"
      ? "waiting for sign-in"
      : job?.status === "needs_space"
        ? "waiting for space"
        : job?.status === "paused"
          ? "paused"
          : "",
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

  // Disk full (F3): while the banner is up, keep an eye on the free space so it can say when there
  // is room again. Retry resumes the job; if the disk is still full it pauses again by itself.
  let space = $state<DiskSpace | null>(null);
  $effect(() => {
    const dest = needsSpace ? job?.spec.dest : null;
    if (!dest) {
      space = null;
      return;
    }
    let stop = false;
    const poll = async () => {
      try {
        const s = await api.diskSpace(dest);
        if (!stop) space = s;
      } catch {
        /* the folder or drive is gone: the banner still works without a figure */
      }
    };
    void poll();
    const timer = setInterval(poll, 4000);
    return () => {
      stop = true;
      clearInterval(timer);
    };
  });

  const fullNeeded = $derived(live?.disk_full?.needed ?? 0);
  const fullFree = $derived(space?.free ?? live?.disk_full?.free ?? 0);
  const fullVolume = $derived(space?.volume ?? "");
  const fullHeadline = $derived(
    fullNeeded > 0 ? fullText(fullVolume, fullNeeded, fullFree) : `${volumeText(fullVolume)} is full.`,
  );
  const roomAgain = $derived(fullNeeded > 0 && fullFree >= fullNeeded);

  async function retrySpace() {
    if (!job) return;
    await act(() => api.resume(job.id));
    app.toast("Trying again.");
  }

  // Fill maps (F6): lanes working on a big file show a strip; clicking a file in the table opens a
  // larger map. The maps are plain data in the store, redrawn when `map_tick` changes.
  let selected = $state<string | null>(null);
  let selectedFor = $state<number | null>(null);
  $effect(() => {
    // A different job, or none: the selection no longer means anything.
    const id = job?.id ?? null;
    if (id !== selectedFor) {
      selectedFor = id;
      selected = null;
    }
  });

  function mapFor(rel: string | null): RangeMap | undefined {
    void live?.map_tick;
    return rel && job ? app.rangeMap(job.id, rel) : undefined;
  }

  function flightsFor(rel: string | null, map: RangeMap | undefined): number[] {
    if (!rel || !map || !live) return [];
    return inFlight(map, live.lanes.filter((l) => l.rel_path === rel).map((l) => l.offset));
  }

  function lanePct(offset: number, total: number, map: RangeMap | undefined): number {
    if (map) return map.count > 0 ? Math.floor((map.doneCount / map.count) * 100) : 0;
    return total > 0 ? Math.min(100, Math.floor((offset / total) * 100)) : 0;
  }

  const largeThreshold = $derived(app.settings?.large_file_threshold ?? 64 * MB);
  const selRow = $derived(selected ? live?.rows.find((r) => r.rel_path === selected) : undefined);
  const selMap = $derived(mapFor(selected));
  const selFlights = $derived(flightsFor(selected, selMap));
  const selLane = $derived(selected && live ? live.lanes.find((l) => l.rel_path === selected) : undefined);

  function pick(rel: string) {
    selected = selected === rel ? null : rel;
  }

  function statusText(q: QueueItem): string {
    switch (q.status) {
      case "queued": return "waiting";
      case "preparing": return "getting ready";
      case "running": return "downloading";
      case "paused": return "paused";
      case "needs_sign_in": return "needs sign-in";
      case "needs_space": return "needs space";
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
      case "needs_space": return "s-bad";
      case "preparing": return "s-wait";
      default: return "s-q";
    }
  }
</script>

<NotifyPrompt />

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

{#if job && needsSpace}
  <div class="alert" role="alert">
    <p>
      <strong>{fullHeadline}</strong>
      {#if roomAgain}
        {size(fullFree)} free now. Press Retry to carry on.
      {:else}
        Free up some space there and press Retry.
        {#if fullFree > 0}<span class="free">{sizeBrief(fullFree)} free now.</span>{/if}
      {/if}
      Everything already saved ({num(filesDone)} {filesDone === 1 ? "file" : "files"}) stays where it is.
    </p>
    <button type="button" class="btn primary" onclick={retrySpace}>Retry</button>
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
      {#if job.status !== "needs_sign_in" && !needsSpace}
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
      <div class="v">{#if stalled}-{:else}{rate(rateNow)[0]} <small>{rate(rateNow)[1]}</small>{/if}</div>
      <div class="sub">{lanesHeader(auto, laneTarget)}</div>
    </div>
    <div>
      <span class="label">Time left</span>
      <div class="v">{isFinite(etaSeconds) ? duration(etaSeconds) : "-"}</div>
      <div class="sub">{isFinite(etaSeconds) ? "at current speed" : laneStatus || (stalled ? "" : "working it out")}</div>
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

  <!-- Chart and lanes share one row that gives way to the file list below it, never the other way
       round: every lane stays on screen (up to 16, two columns above 8) without scrolling. -->
  <div class="grid-2 mid" class:many={dense} style:min-height="{midMin}px">
    <div class="panel col chart-panel">
      <div class="panel-h"><span class="label">Speed · last 2 min</span><span class="meta">peak {num(peak / MB, 1)} MB/s</span></div>
      {#if live}<Chart hist={live.hist} />{/if}
    </div>
    <div class="panel col lanes-panel">
      <div class="panel-h"><span class="label">Parallel lanes</span><span class="meta">{lanesHeader(auto, laneTarget)}</span></div>
      <div class="lanes" class:two={dense} class:dense style:--row-min="{laneMin}px">
        {#each shownLanes as lane, i}
          {@const waiting = laneStatus !== ""}
          {@const map = mapFor(lane.rel_path)}
          {@const closing = i >= laneTarget}
          <div class="lane" class:waiting class:closing>
            <span class="id">L{i + 1}</span>
            <div class="lane-body">
              <div class="line">
                <span class="file" title={lane.rel_path ?? ""}>{lane.rel_path ?? (closing ? "closing" : "idle")}</span>
                <span class="rng">
                  {#if waiting}
                    {laneStatus}
                  {:else if lane.rel_path}
                    {#if dense}{lanePct(lane.offset, lane.size, map)}%{:else if map}{lanePct(lane.offset, lane.size, map)}% of {size(lane.size)}{:else}{num(lane.offset / MB, 0)} MB / {size(lane.size)}{/if}
                  {/if}
                </span>
              </div>
              {#if map && lane.rel_path}
                <FillMap {map} tick={live?.map_tick ?? 0} flights={flightsFor(lane.rel_path, map)} />
              {:else}
                <div class="bar"><i style:width="{lane.rel_path && lane.size > 0 ? ((lane.offset / lane.size) * 100).toFixed(1) : 0}%"></i></div>
              {/if}
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
              <tr class:sel={r.rel_path === selected}>
                <td class="name" title={r.rel_path}>
                  <button type="button" class="rowbtn" onclick={() => pick(r.rel_path)} aria-pressed={r.rel_path === selected} aria-label="{r.rel_path}: show details">
                    {#if r.size !== null && isRanged(r.size, largeThreshold)}
                      <svg class="mm" viewBox="0 0 12 8" aria-hidden="true"><rect x="0" y="0" width="2.4" height="8" /><rect x="3.2" y="0" width="2.4" height="8" /><rect x="6.4" y="0" width="2.4" height="8" opacity=".35" /><rect x="9.6" y="0" width="2.4" height="8" opacity=".35" /></svg>
                    {/if}
                    <span>{r.rel_path}</span>
                  </button>
                </td>
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

  {#if selected}
    <div class="panel col selected" aria-label="Selected file">
      <div class="panel-h">
        <span class="label">Selected file</span>
        <button type="button" class="link" onclick={() => (selected = null)}>Close</button>
      </div>
      <div class="sel-body body">
        <div class="sel-name" title={selected}>{selected}</div>
        <div class="sel-facts">
          <span>{selRow && selRow.size !== null ? size(selRow.size) : selMap ? size(selMap.size) : "size unknown"}</span>
          {#if selRow}<span class="state {stateClass(selRow.state)}">{stateText(selRow.state)}</span>{/if}
          {#if selRow?.check}<span class="why" title={selRow.check}>{selRow.check}</span>{/if}
        </div>
        {#if selMap}
          <div class="sel-map-wrap">
            <FillMap map={selMap} tick={live?.map_tick ?? 0} flights={selFlights} mode="grid" />
          </div>
          <div class="sel-sum">{mapSummary(selMap, selFlights)}</div>
          <ul class="legend" aria-label="Key to the map">
            <li><i class="k wait"></i>waiting</li>
            <li><i class="k flight"></i>downloading</li>
            <li><i class="k done"></i>saved</li>
            <li><i class="k retried"></i>fetched again</li>
          </ul>
        {:else if selLane && selLane.size > 0}
          <p class="sel-note">Smaller than {size(largeThreshold)}, so it comes in one piece. {lanePct(selLane.offset, selLane.size, undefined)}% so far.</p>
        {:else if selRow && selRow.size !== null && isRanged(selRow.size, largeThreshold)}
          <p class="sel-note">This file was split into pieces, but its map is no longer kept. Only the 500 most recent files keep one.</p>
        {:else}
          <p class="sel-note">Files under {size(largeThreshold)} come in one piece, so there is no map to show. Bigger files are split into ranges and filled in as they arrive.</p>
        {/if}
      </div>
    </div>
  {:else}
    <div class="panel col" data-tour="queue">
      <div class="panel-h"><span class="label">Queue</span><span class="meta">runs in order</span></div>
      <div class="queue body">
        {#if job}
          <div class="qitem active">
            <span class="t">{job.title}</span><span class="sz">{size(job.est_bytes)}</span><span class="p">{job.path_label}</span>
            {#if job.status === "paused" || job.status === "needs_sign_in" || job.status === "needs_space"}
              <span class="q-actions"><span class="state {statusClass(job)}">{statusText(job)}</span></span>
            {/if}
          </div>
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
  {/if}
</div>

<style>
  .job-head { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 14px 24px; align-items: end; }
  .job-head h2 { margin: 2px 0 4px; font: 600 22px/1.2 var(--f-display); text-wrap: balance; }
  .path { font: 12px var(--f-mono); color: var(--muted); overflow-wrap: anywhere; }
  .actions { display: flex; gap: 8px; flex-wrap: wrap; }
  .free { font-family: var(--f-mono); color: var(--muted); }

  .readout { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); border: 1px solid var(--line); border-radius: 4px; background: var(--bg); }
  .readout > div { padding: 12px 16px; border-right: 1px solid var(--line); min-width: 0; }
  .readout > div:last-child { border-right: 0; }
  .v { font: 500 22px/1.25 var(--f-mono); font-variant-numeric: tabular-nums; margin-top: 6px; white-space: nowrap; }
  .v small { font-size: 13px; color: var(--muted); }
  .sub { font: 12px var(--f-mono); color: var(--muted); }

  /* The chart and lanes row gives way to the files row when space runs short (see app.css). Rows of
     the lane list share the panel's height between a floor and a cap, so 4 lanes sit comfortably
     and 16 still fit; the speed chart stretches to the same height. */
  .mid { grid-template-rows: minmax(0, 1fr); }
  .mid.many { grid-template-columns: minmax(0, 1fr) minmax(0, 1.35fr); }
  .lanes { flex: 1 1 auto; min-height: 0; overflow: hidden; display: grid; align-content: start; grid-auto-rows: minmax(var(--row-min, 32px), var(--row-max, 38px)); }
  .lanes.two { grid-template-columns: repeat(2, minmax(0, 1fr)); --row-max: 28px; }
  .lanes.two .lane:nth-child(odd) { border-right: 1px solid var(--line); }
  .fill-row { grid-template-rows: minmax(0, 1fr); }
  .lane { --fm-h: 8px; display: grid; grid-template-columns: 26px minmax(0, 1fr); gap: 8px; align-items: center; padding: 4px 14px; border-bottom: 1px solid var(--line); min-width: 0; min-height: 0; overflow: hidden; }
  .lanes:not(.two) .lane:last-child { border-bottom: 0; }
  .lanes.dense .lane { --fm-h: 4px; padding: 1px 10px; gap: 6px; grid-template-columns: 22px minmax(0, 1fr); }
  .lane .id { font: 500 11px var(--f-mono); color: var(--signal); }
  .lane.closing .id { color: var(--muted); }
  .lane-body { min-width: 0; display: grid; gap: 3px; }
  .lanes.dense .lane-body { gap: 1px; }
  .lanes.dense .lane .line { line-height: 14px; }
  .lane .line { display: flex; gap: 10px; align-items: baseline; min-width: 0; }
  .lane .file { flex: 1 1 auto; min-width: 0; font: 12px var(--f-mono); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .lanes.dense .lane .file, .lanes.dense .lane .rng { font-size: 11px; }
  .lane .file[title=""] { color: var(--muted); }
  .lane .rng { flex: none; font: 11px var(--f-mono); color: var(--muted); font-variant-numeric: tabular-nums; }
  .lane .bar { height: 3px; }
  .lane.waiting .id { color: var(--info); }
  .lane.waiting .bar > i { background: var(--info); }

  .check { font: 11px var(--f-mono); color: var(--muted); max-width: 220px; overflow: hidden; text-overflow: ellipsis; }
  .panel-empty { margin: 0; padding: 18px 14px; color: var(--muted); font-size: 13px; }

  .rowbtn { display: flex; align-items: center; gap: 8px; width: 100%; max-width: 100%; padding: 0; border: 0; background: none; color: inherit; font: inherit; text-align: left; cursor: pointer; }
  .rowbtn span { min-width: 0; overflow: hidden; text-overflow: ellipsis; }
  .rowbtn:hover span { text-decoration: underline; text-underline-offset: 3px; }
  .mm { flex: none; width: 12px; height: 8px; fill: var(--signal); }
  tr.sel td { background: color-mix(in srgb, var(--signal) 9%, transparent); }
  tr.sel .rowbtn span { color: var(--signal); }

  .selected { min-width: 0; }
  .sel-body { display: flex; flex-direction: column; gap: 10px; padding: 12px 14px; min-height: 0; overflow: auto; }
  .sel-name { font: 12px var(--f-mono); overflow-wrap: anywhere; }
  .sel-facts { display: flex; flex-wrap: wrap; gap: 4px 14px; align-items: baseline; font: 12px var(--f-mono); color: var(--muted); }
  .sel-facts .why { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 100%; }
  .sel-map-wrap { flex: 1 1 80px; min-height: 60px; display: flex; flex-direction: column; }
  .sel-map-wrap > :global(.fm) { flex: 1 1 0; }
  .sel-sum { font: 12px var(--f-mono); color: var(--fg); }
  .sel-note { margin: 0; font-size: 13px; color: var(--muted); max-width: 52ch; }
  .legend { display: flex; flex-wrap: wrap; gap: 4px 16px; margin: 0; padding: 0; list-style: none; font: 11px var(--f-mono); color: var(--muted); }
  .legend li { display: inline-flex; align-items: center; gap: 6px; }
  .k { display: inline-block; width: 10px; height: 10px; box-sizing: border-box; background: var(--line); }
  .k.flight { background: color-mix(in srgb, var(--signal) 40%, var(--line)); }
  .k.done { background: var(--signal); }
  .k.retried { background: color-mix(in srgb, var(--signal) 40%, var(--line)); border: 1px solid var(--signal); }

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

  @media (max-height: 640px) {
    .path { display: none; }
  }
  @media (max-height: 720px) {
    .v { font-size: 19px; margin-top: 3px; }
    .readout > div { padding: 8px 14px; }
    .job-head h2 { font-size: 19px; margin: 0 0 2px; }
  }
  @media (max-width: 800px) {
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
