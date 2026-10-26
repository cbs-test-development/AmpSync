import { api, on, toCmdError } from "./api";
import { inFlight, inferChunk, isRanged, RangeMap, rangeCount } from "./fillmap";
import { MAX_LANES, type LaneMark } from "./lanes";
import type { SignInPhase } from "./signin";
import type {
  AppInfo,
  ContributorList,
  EngineEvent,
  IncomingLink,
  JobEventPayload,
  QueueItem,
  SessionInfo,
  Settings,
  UpdateInfo,
  ViewId,
} from "./types";

export type Theme = "system" | "dark" | "light";

export interface LaneState {
  rel_path: string | null;
  offset: number;
  size: number;
}

export interface FileRow {
  rel_path: string;
  size: number | null;
  state: "running" | "ok" | "skipped" | "retrying" | "failed";
  check: string;
}

export interface HistSample {
  v: number;
  throttled: boolean;
  /** Lane-count changes that happened during this second (F4), drawn as a marker on the chart. */
  marks?: LaneMark[];
}

export const HIST_LEN = 120;

export interface LiveJob {
  bytes_done: number;
  bytes_total: number;
  rate_bps: number;
  files_total: number;
  verified: number;
  verified_hash: number;
  skipped: number;
  failed: number;
  lanes: LaneState[];
  rows: FileRow[];
  hist: HistSample[];
  paused: boolean;
  waiting_sign_in: boolean;
  /** Set while the job is paused because the destination disk is full (D21). */
  disk_full: { path: string; needed: number; free: number } | null;
  /** The lane count automatic tuning (or the fixed setting) is working towards. */
  lane_target: number;
  /** Lane changes not yet attached to a chart sample. */
  pending_marks: LaneMark[];
  /** Bumped at most once per animation frame when a fill map changed (the maps are not reactive). */
  map_tick: number;
  throttled_until: number;
  pending_throttle: boolean;
  last_progress: number;
}

/** Engine defaults, for when the settings have not loaded. */
const MiB = 1024 * 1024;
const DEFAULT_RANGE = 16 * MiB;
const DEFAULT_LARGE = 64 * MiB;

function emptyLive(laneTarget: number): LiveJob {
  return {
    bytes_done: 0,
    bytes_total: 0,
    rate_bps: 0,
    files_total: 0,
    verified: 0,
    verified_hash: 0,
    skipped: 0,
    failed: 0,
    // Lane slots are stable (0..15); how many are shown follows `lane_target`.
    lanes: Array.from({ length: MAX_LANES }, () => ({ rel_path: null, offset: 0, size: 0 })),
    rows: [],
    hist: Array.from({ length: HIST_LEN }, () => ({ v: 0, throttled: false })),
    paused: false,
    waiting_sign_in: false,
    disk_full: null,
    lane_target: laneTarget,
    pending_marks: [],
    map_tick: 0,
    throttled_until: 0,
    pending_throttle: false,
    last_progress: 0,
  };
}

const MAX_ROWS = 500;

class Store {
  view = $state<ViewId>("transfers");
  theme = $state<Theme>("system");
  settings = $state<Settings | null>(null);
  queue = $state<QueueItem[]>([]);
  sessions = $state<SessionInfo[]>([]);
  live = $state<Record<number, LiveJob>>({});
  info = $state<AppInfo | null>(null);
  contributors = $state<ContributorList>({ repo: "", items: [] });
  update = $state<UpdateInfo | null>(null);
  toastText = $state<string | null>(null);
  tourOpen = $state(false);
  closePrompt = $state(false);
  /** Whether a download was running when the close prompt opened. */
  closeActive = $state(false);
  /** Host of the sign-in window that is open right now, if any. */
  signInOpen = $state<string | null>(null);
  /** What that sign-in is waiting on: the choice dialog, the app window or the browser. */
  signInPhase = $state<SignInPhase | null>(null);
  /** Host of the sign-in question being asked in the dialog, if any (D26). */
  signInChoose = $state<string | null>(null);
  ready = $state(false);
  /** A SharePoint link that arrived by paste, drop or the tray; New download opens it. */
  incoming = $state<{ link: string; n: number } | null>(null);
  /** Short inline message under the top bar, e.g. for a paste that is not a SharePoint folder link. */
  notice = $state<string | null>(null);

  private toastTimer: ReturnType<typeof setTimeout> | undefined;
  private noticeTimer: ReturnType<typeof setTimeout> | undefined;
  private incomingSeq = 0;
  private rowIndex = new Map<number, Map<string, FileRow>>();
  /** Fill maps of the big files of each job (F6). Plain data, redrawn on `LiveJob.map_tick`. */
  private maps = new Map<number, Map<string, RangeMap>>();
  /** `done_ranges` of each map's `file_started`, to rebuild it if the range size guess was wrong. */
  private seeds = new Map<number, Map<string, number[]>>();
  private tickQueued = new Set<number>();

  /** The job shown in the Transfers view: the running one, else the first waiting one. */
  get active(): QueueItem | undefined {
    const order: QueueItem["status"][] = ["running", "needs_sign_in", "needs_space", "preparing", "paused"];
    for (const s of order) {
      const f = this.queue.find((q) => q.status === s);
      if (f) return f;
    }
    return undefined;
  }

  get waiting(): QueueItem[] {
    const a = this.active;
    return this.queue.filter((q) => q.id !== a?.id && (q.status === "queued" || q.status === "paused"));
  }

  get finished(): QueueItem[] {
    return this.queue.filter((q) => q.status === "done" || q.status === "failed" || q.status === "cancelled").reverse();
  }

  get expired(): SessionInfo[] {
    return this.sessions.filter((s) => s.status === "expired");
  }

  go(v: ViewId) {
    this.view = v;
  }

  toast(text: string, ms = 3200) {
    this.toastText = text;
    clearTimeout(this.toastTimer);
    this.toastTimer = setTimeout(() => (this.toastText = null), ms);
  }

  setNotice(text: string | null, ms = 8000) {
    this.notice = text;
    clearTimeout(this.noticeTimer);
    if (text) this.noticeTimer = setTimeout(() => (this.notice = null), ms);
  }

  /**
   * Text from a paste, a drop or the tray's "Download link from clipboard". A SharePoint folder
   * link opens New download and starts listing; anything else gets a short inline notice. The
   * tour and the close question keep the keyboard to themselves.
   */
  async receiveLink(text: string) {
    if (!this.ready || this.tourOpen || this.closePrompt) return;
    try {
      const info = await api.parseLink(text);
      this.setNotice(null);
      this.incoming = { link: info.link, n: ++this.incomingSeq };
      this.go("new");
    } catch (e) {
      this.setNotice(toCmdError(e).message);
    }
  }

  setTheme(t: Theme) {
    this.theme = t;
    const root = document.documentElement;
    if (t === "system") root.removeAttribute("data-theme");
    else root.setAttribute("data-theme", t);
    try {
      localStorage.setItem("ampsync-theme", t);
    } catch {
      /* private mode: the choice just will not persist */
    }
  }

  async saveSettings(patch: Partial<Settings>) {
    if (!this.settings) return;
    const next = { ...this.settings, ...patch };
    this.settings = next;
    try {
      this.settings = await api.setSettings(next);
    } catch (e) {
      this.toast(toCmdError(e).message);
    }
  }

  async refreshSessions() {
    try {
      this.sessions = await api.sessions();
    } catch {
      /* keychain unavailable: leave the list as is */
    }
  }

  /** Lanes a job starts with: 4 under automatic tuning (D22, no event is sent for it), else the setting. */
  private initialLanes() {
    return this.settings?.auto_tune ? 4 : (this.settings?.parallel_files ?? 4);
  }

  private liveFor(id: number): LiveJob {
    if (!this.live[id]) this.live[id] = emptyLive(this.initialLanes());
    return this.live[id];
  }

  /** The fill map of a file, if it is one of the big, ranged ones. */
  rangeMap(jobId: number, rel: string): RangeMap | undefined {
    return this.maps.get(jobId)?.get(rel);
  }

  private mapsOf(id: number): Map<string, RangeMap> {
    let m = this.maps.get(id);
    if (!m) this.maps.set(id, (m = new Map()));
    return m;
  }

  private dropJob(id: number) {
    delete this.live[id];
    this.rowIndex.delete(id);
    this.maps.delete(id);
    this.seeds.delete(id);
  }

  /** Redraw the maps once per animation frame, however many `range_done` events arrived. */
  private scheduleTick(id: number) {
    if (this.tickQueued.has(id)) return;
    this.tickQueued.add(id);
    const run = () => {
      this.tickQueued.delete(id);
      const job = this.live[id];
      if (job) job.map_tick++;
    };
    if (typeof requestAnimationFrame === "function") requestAnimationFrame(run);
    else setTimeout(run, 16);
  }

  /** Lane offsets for one file: the ranges being fetched right now. */
  private flightsOf(id: number, rel: string, map: RangeMap): number[] {
    const job = this.live[id];
    if (!job) return [];
    return inFlight(map, job.lanes.filter((l) => l.rel_path === rel).map((l) => l.offset));
  }

  private startMap(id: number, rel: string, size: number, done: number[]) {
    const maps = this.mapsOf(id);
    const seeds = this.seeds.get(id) ?? new Map<string, number[]>();
    this.seeds.set(id, seeds);
    const range = this.settings?.range_size ?? DEFAULT_RANGE;
    if (!isRanged(size, this.settings?.large_file_threshold ?? DEFAULT_LARGE)) {
      maps.delete(rel);
      seeds.delete(rel);
      return;
    }
    // Range size: the configured one, or double after the request budget was hit (D22); the first
    // `range_done` says for sure. Finished ranges beyond the first guess's count point at the other.
    const top = done.reduce((a, b) => Math.max(a, b), -1);
    const chunk = [range, range * 2].find((c) => rangeCount(size, c) > top) ?? range;
    const map = new RangeMap(size, rangeCount(size, chunk), chunk);
    for (const i of done) map.markDone(i);
    maps.set(rel, map); // a second `file_started` for a path starts its map over
    seeds.set(rel, done);
  }

  private rangeDone(id: number, rel: string, index: number, count: number, size: number) {
    const maps = this.mapsOf(id);
    let map = maps.get(rel);
    if (!map || map.count !== count) {
      const range = this.settings?.range_size ?? DEFAULT_RANGE;
      const chunk = inferChunk(map?.size ?? size, count, [range, range * 2]);
      const next = new RangeMap(map?.size ?? size, count, chunk);
      for (const i of this.seeds.get(id)?.get(rel) ?? []) next.markDone(i);
      if (map) for (let i = 0; i < Math.min(map.count, count); i++) if (chunk === map.chunk && map.isDone(i)) next.markDone(i);
      maps.set(rel, (map = next));
    }
    map.markDone(index, performance.now());
    this.scheduleTick(id);
  }

  private handleEvent(id: number, ev: EngineEvent) {
    const job = this.liveFor(id);
    const index = this.rowIndex.get(id) ?? new Map<string, FileRow>();
    this.rowIndex.set(id, index);
    const touch = (row: FileRow) => {
      const known = index.get(row.rel_path);
      if (known) {
        Object.assign(known, row);
        const i = job.rows.indexOf(known);
        if (i > 0) {
          job.rows.splice(i, 1);
          job.rows.unshift(known);
        }
      } else {
        job.rows.unshift(row);
        index.set(row.rel_path, job.rows[0]); // the reactive proxy, so later edits are seen
        if (job.rows.length > MAX_ROWS) {
          const gone = job.rows.pop();
          if (gone) {
            index.delete(gone.rel_path);
            this.maps.get(id)?.delete(gone.rel_path);
            this.seeds.get(id)?.delete(gone.rel_path);
          }
        }
      }
    };
    switch (ev.type) {
      case "planned":
        // A new run of this job (also after "Try again"): the old maps describe an earlier run.
        this.maps.delete(id);
        this.seeds.delete(id);
        job.disk_full = null;
        // Files that were already up to date arrive as a count here, not as per-file events.
        job.files_total = ev.files + ev.skipped;
        job.skipped = ev.skipped;
        job.bytes_total = ev.bytes;
        break;
      case "file_started":
        if (ev.lane < job.lanes.length) job.lanes[ev.lane] = { rel_path: ev.rel_path, offset: 0, size: ev.size };
        touch({ rel_path: ev.rel_path, size: ev.size, state: "running", check: "" });
        this.startMap(id, ev.rel_path, ev.size, ev.done_ranges ?? []);
        this.scheduleTick(id);
        break;
      case "range_done":
        this.rangeDone(id, ev.rel_path, ev.index, ev.count, index.get(ev.rel_path)?.size ?? 0);
        break;
      case "lanes_changed": {
        job.lane_target = ev.lanes;
        job.pending_marks.push({ lanes: ev.lanes, reason: ev.reason });
        break;
      }
      case "disk_full":
        job.disk_full = { path: ev.path, needed: ev.needed, free: ev.free };
        job.rate_bps = 0;
        break;
      case "progress":
        job.bytes_done = ev.bytes_done;
        job.bytes_total = ev.bytes_total;
        job.rate_bps = ev.rate_bps;
        job.last_progress = Date.now();
        job.paused = false;
        job.waiting_sign_in = false;
        break;
      case "lane_progress": {
        const lane = job.lanes[ev.lane];
        if (lane) {
          lane.rel_path = ev.rel_path;
          lane.offset = ev.offset;
          lane.size = ev.size;
        }
        break;
      }
      case "file_verified": {
        job.verified += 1;
        if (ev.by === "hash") job.verified_hash += 1;
        for (const l of job.lanes) if (l.rel_path === ev.rel_path) l.rel_path = null;
        const map = this.rangeMap(id, ev.rel_path);
        if (map) {
          map.complete();
          this.scheduleTick(id);
        }
        const known = index.get(ev.rel_path);
        touch({
          rel_path: ev.rel_path,
          size: known?.size ?? null,
          state: "ok",
          check: ev.by === "hash" ? "fingerprint match" : "size and version",
        });
        break;
      }
      case "file_failed": {
        if (!ev.will_retry) job.failed += 1;
        const failing = this.rangeMap(id, ev.rel_path);
        if (failing && ev.will_retry) {
          // The ranges being fetched right now are the ones that failed: outline them on the map.
          for (const r of this.flightsOf(id, ev.rel_path, failing)) failing.markRetried(r);
          this.scheduleTick(id);
        }
        // A lane that will retry carries on with the same file.
        if (!ev.will_retry) for (const l of job.lanes) if (l.rel_path === ev.rel_path) l.rel_path = null;
        const known = index.get(ev.rel_path);
        touch({
          rel_path: ev.rel_path,
          size: known?.size ?? null,
          state: ev.will_retry ? "retrying" : "failed",
          check: ev.error,
        });
        break;
      }
      case "throttled":
        job.pending_throttle = true;
        job.throttled_until = Date.now() + ev.retry_after_ms;
        break;
      case "auth_expired":
        job.waiting_sign_in = true;
        job.rate_bps = 0;
        break;
      case "paused":
        job.paused = true;
        job.rate_bps = 0;
        break;
      case "resumed":
        job.paused = false;
        job.disk_full = null;
        break;
      case "finished":
        // The summary is exact; it also repairs counters if events were dropped on the way.
        job.rate_bps = 0;
        job.verified = ev.downloaded;
        job.skipped = ev.skipped;
        job.failed = ev.failed;
        break;
    }
  }

  /** One chart sample per second for every job that is live. */
  private sample() {
    const now = Date.now();
    for (const job of Object.values(this.live)) {
      const stale = now - job.last_progress > 3000;
      const v = job.paused || job.waiting_sign_in || stale ? 0 : job.rate_bps;
      const throttled = job.pending_throttle || now < job.throttled_until;
      job.pending_throttle = false;
      const marks = job.pending_marks.length > 0 ? job.pending_marks.splice(0) : undefined;
      job.hist.push(marks ? { v, throttled, marks } : { v, throttled });
      if (job.hist.length > HIST_LEN) job.hist.shift();
    }
  }

  async init() {
    try {
      this.theme = (localStorage.getItem("ampsync-theme") as Theme) || "system";
    } catch {
      this.theme = "system";
    }
    this.setTheme(this.theme);

    await on<QueueItem[]>("queue-changed", (q) => {
      this.queue = q;
      const ids = new Set(q.map((i) => i.id));
      for (const k of Object.keys(this.live)) if (!ids.has(Number(k))) this.dropJob(Number(k));
    });
    await on<JobEventPayload>("job-event", (p) => this.handleEvent(p.job_id, p.event));
    await on<null>("sessions-changed", () => void this.refreshSessions());
    await on<{ host: string; state: string }>("signin-state", (p) => {
      const open = p.state !== "closed";
      this.signInOpen = open ? p.host : null;
      this.signInPhase = open ? (p.state as SignInPhase) : null;
      if (p.state !== "choosing") this.signInChoose = null;
    });
    await on<{ host: string }>("signin-choose", (p) => (this.signInChoose = p.host));
    await on<boolean>("close-requested", (active) => {
      this.closeActive = active;
      this.closePrompt = true;
    });
    await on<UpdateInfo | null>("update-available", (u) => (this.update = u));
    await on<IncomingLink>("incoming-link", (p) => void this.receiveLink(p.text));
    // A click on a finish, failure or sign-in notification: the backend has raised the window.
    await on<{ kind: string }>("notification-click", () => this.go("transfers"));

    const [settings, queue, info, contributors, update] = await Promise.all([
      api.getSettings(),
      api.getQueue(),
      api.appInfo(),
      api.contributors(),
      api.getUpdate(),
    ]);
    this.settings = settings;
    this.queue = queue;
    this.info = info;
    this.contributors = contributors;
    this.update = update;
    await this.refreshSessions();
    // A question that was asked before this page was listening (the window was hidden).
    this.signInChoose = (await api.pendingSignInChoice().catch(() => null)) ?? this.signInChoose;
    this.ready = true;
    setInterval(() => this.sample(), 1000);
    if (!settings.tour_seen) this.startTour();
  }

  tourReturn: ViewId = "transfers";

  startTour() {
    this.tourReturn = this.view;
    this.tourOpen = true;
  }

  async endTour(skipped: boolean) {
    this.tourOpen = false;
    this.go("transfers");
    if (skipped) this.toast("Tour skipped. You can replay it from Settings.");
    try {
      this.settings = await api.markTourSeen();
    } catch {
      /* not critical */
    }
  }
}

export const app = new Store();
