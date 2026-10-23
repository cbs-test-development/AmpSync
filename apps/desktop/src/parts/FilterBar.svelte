<script lang="ts">
  import { num, size } from "../core/format";
  import { emptyState, splitChips, type ExtStat, type FilterResult, type FilterState } from "../core/filter";

  /**
   * The filter bar above the folder picker: name search, file-type chips (top 8 by total size, the
   * rest under "More"), size and modified filters, and the two buttons that act on the matches.
   * It only edits `filter`; the parent runs it and passes the result back.
   */
  let {
    filter = $bindable(),
    stats,
    active,
    result,
    onselect,
    onclear,
  }: {
    filter: FilterState;
    stats: ExtStat[];
    active: boolean;
    result: FilterResult | null;
    onselect: () => void;
    onclear: () => void;
  } = $props();

  const chips = $derived(splitChips(stats));
  const moreOn = $derived(chips.more.filter((s) => filter.exts.includes(s.key)).length);
  let moreOpen = $state(false);
  let moreBox: HTMLDivElement | undefined = $state();
  let search: HTMLInputElement | undefined = $state();

  const extLabel = (key: string) => (key === "" ? "no ext." : `.${key}`);
  const isOn = (key: string) => filter.exts.includes(key);

  function toggle(key: string) {
    filter.exts = isOn(key) ? filter.exts.filter((k) => k !== key) : [...filter.exts, key];
  }

  function reset() {
    Object.assign(filter, emptyState());
    moreOpen = false;
    search?.focus();
  }

  function onSearchKey(e: KeyboardEvent) {
    if (e.key === "Escape" && filter.query !== "") {
      filter.query = "";
      e.stopPropagation();
    }
  }

  function onWindowDown(e: PointerEvent) {
    if (moreOpen && moreBox && !moreBox.contains(e.target as Node)) moreOpen = false;
  }

  function onWindowKey(e: KeyboardEvent) {
    if (moreOpen && e.key === "Escape") {
      moreOpen = false;
      moreBox?.querySelector<HTMLButtonElement>(".more-btn")?.focus();
    }
  }

  const noMatch = $derived(result !== null && result.count === 0);
</script>

<svelte:window onpointerdown={onWindowDown} onkeydown={onWindowKey} />

<div class="fbar" role="search" aria-label="Filter the file list">
  <div class="line">
    <div class="search">
      <svg viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.6" aria-hidden="true"><circle cx="8.5" cy="8.5" r="5" /><path d="M12.4 12.4L17 17" /></svg>
      <input
        class="field"
        type="text"
        bind:this={search}
        bind:value={filter.query}
        onkeydown={onSearchKey}
        spellcheck="false"
        autocomplete="off"
        aria-label="Search file and folder names"
        placeholder="Search names. * and ? work as wildcards"
      />
      {#if filter.query}
        <button type="button" class="x" aria-label="Clear the search" onclick={() => { filter.query = ""; search?.focus(); }}>
          <svg viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.8" aria-hidden="true"><path d="M5 5l10 10M15 5L5 15" /></svg>
        </button>
      {/if}
    </div>

    <div class="ctl">
      <label class="label" for="f-size">Size</label>
      <select class="field" id="f-size" bind:value={filter.size}>
        <option value="any">Any size</option>
        <option value="100mb">Over 100 MB</option>
        <option value="1gb">Over 1 GB</option>
        <option value="custom">Custom range</option>
      </select>
      {#if filter.size === "custom"}
        <span class="range">
          <input class="field num" inputmode="decimal" aria-label="Smallest size" placeholder="from" bind:value={filter.minCustom} />
          <span aria-hidden="true">to</span>
          <input class="field num" inputmode="decimal" aria-label="Largest size" placeholder="up to" bind:value={filter.maxCustom} />
          <select class="field unit" aria-label="Size unit" bind:value={filter.unit}>
            <option>MB</option>
            <option>GB</option>
          </select>
        </span>
      {/if}
    </div>

    <div class="ctl">
      <label class="label" for="f-mod">Modified</label>
      <select class="field" id="f-mod" bind:value={filter.modified}>
        <option value="any">Any time</option>
        <option value="7d">Last 7 days</option>
        <option value="30d">Last 30 days</option>
        <option value="since">Since a date</option>
      </select>
      {#if filter.modified === "since"}
        <input class="field date" type="date" aria-label="Modified since" bind:value={filter.sinceDate} />
      {/if}
    </div>
  </div>

  {#if stats.length > 0}
    <div class="chips" role="group" aria-label="File types">
      <span class="label">Type</span>
      {#each chips.top as s (s.key)}
        <button type="button" class="chip" aria-pressed={isOn(s.key)} title="{num(s.files)} {s.files === 1 ? 'file' : 'files'}" onclick={() => toggle(s.key)}>
          <span class="ext">{extLabel(s.key)}</span><span class="sz">{size(s.bytes)}</span>
        </button>
      {/each}
      {#if chips.more.length > 0}
        <div class="more" bind:this={moreBox}>
          <button type="button" class="chip more-btn" class:on={moreOn > 0} aria-expanded={moreOpen} aria-haspopup="true" onclick={() => (moreOpen = !moreOpen)}>
            <span class="ext">More</span><span class="sz">{moreOn > 0 ? `${moreOn} on` : chips.more.length}</span>
          </button>
          {#if moreOpen}
            <div class="menu" role="group" aria-label="More file types">
              {#each chips.more as s (s.key)}
                <label class="item">
                  <input type="checkbox" checked={isOn(s.key)} onchange={() => toggle(s.key)} />
                  <span class="ext">{extLabel(s.key)}</span>
                  <span class="sz">{size(s.bytes)}</span>
                </label>
              {/each}
            </div>
          {/if}
        </div>
      {/if}
    </div>
  {/if}

  {#if active}
    <div class="strip" role="status">
      <span class="state">
        {#if result === null}Filtering...{:else if noMatch}No file matches.{:else}Showing only the {num(result.count)} matching {result.count === 1 ? "file" : "files"}.{/if}
      </span>
      <span class="acts">
        <button type="button" class="btn sm" onclick={onselect} disabled={!result || noMatch}>Select matching</button>
        <button type="button" class="btn sm" onclick={onclear} disabled={!result || noMatch}>Clear matching</button>
        <button type="button" class="btn sm ghost" onclick={reset}>Reset filters</button>
      </span>
    </div>
  {/if}
</div>

<style>
  .fbar { display: grid; gap: 10px; padding: 12px 14px; border-bottom: 1px solid var(--line); background: var(--panel); }
  .line { display: flex; flex-wrap: wrap; gap: 10px 16px; align-items: flex-end; }

  .search { position: relative; flex: 1 1 240px; min-width: 200px; }
  .search > svg { position: absolute; left: 10px; top: 50%; width: 15px; height: 15px; margin-top: -7.5px; color: var(--muted); pointer-events: none; }
  .search .field { padding: 7px 32px 7px 32px; }
  .x { position: absolute; right: 4px; top: 50%; width: 26px; height: 26px; margin-top: -13px; display: grid; place-items: center; background: none; border: 0; border-radius: 3px; color: var(--muted); cursor: pointer; }
  .x:hover { color: var(--fg); background: var(--raise); }
  .x svg { width: 12px; height: 12px; }

  .ctl { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .ctl .field { width: auto; padding: 6px 10px; font-size: 13px; }
  .range { display: inline-flex; align-items: center; gap: 6px; color: var(--muted); font-size: 12px; }
  .field.num { width: 76px; font-family: var(--f-mono); }
  .field.unit { padding-right: 6px; }
  .field.date { font-family: var(--f-mono); }

  .chips { display: flex; flex-wrap: wrap; align-items: center; gap: 6px 6px; }
  .chips > .label { margin-right: 6px; }
  .chip {
    display: inline-flex; align-items: baseline; gap: 7px; padding: 3px 9px; border-radius: 3px; cursor: pointer;
    border: 1px solid var(--line); background: var(--raise); font: 12px var(--f-mono); color: var(--fg);
  }
  .chip:hover { border-color: var(--muted); }
  .chip[aria-pressed="true"], .chip.on {
    border-color: var(--signal); background: color-mix(in srgb, var(--signal) 14%, var(--raise));
  }
  .chip[aria-pressed="true"] .ext, .chip.on .ext { color: var(--signal); }
  .chip .sz { color: var(--muted); font-size: 11px; }

  .more { position: relative; }
  .menu {
    position: absolute; z-index: 20; top: calc(100% + 4px); left: 0; width: 230px; max-height: 240px; overflow-y: auto;
    background: var(--raise); border: 1px solid var(--line); border-top: 2px solid var(--signal); border-radius: 4px;
    box-shadow: 0 10px 26px rgba(0, 0, 0, .3); padding: 4px 0;
  }
  .item { display: grid; grid-template-columns: auto minmax(0, 1fr) auto; gap: 8px; align-items: center; padding: 5px 12px; cursor: pointer; font: 12px var(--f-mono); }
  .item:hover { background: var(--panel); }
  .item input { accent-color: var(--signal); margin: 0; }
  .item .sz { color: var(--muted); font-size: 11px; }
  .ext { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

  .strip {
    display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 8px 16px;
    padding: 7px 10px; border: 1px solid var(--line); border-left: 3px solid var(--signal); border-radius: 3px; background: var(--raise);
  }
  .state { font: 12px var(--f-mono); color: var(--muted); }
  .acts { display: flex; flex-wrap: wrap; gap: 8px; }
  .btn.sm { padding: 4px 11px; font-size: 12px; }
</style>
