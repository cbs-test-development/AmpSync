import { api, on, toCmdError } from "./api";
import type {
  AppInfo,
  ContributorList,
  EngineEvent,
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
  throttled_until: number;
  pending_throttle: boolean;
  last_progress: number;
}

function emptyLive(lanes: number): LiveJob {
  return {
    bytes_done: 0,
    bytes_total: 0,
    rate_bps: 0,
    files_total: 0,
    verified: 0,
    verified_hash: 0,
    skipped: 0,
    failed: 0,
    lanes: Array.from({ length: lanes }, () => ({ rel_path: null, offset: 0, size: 0 })),
    rows: [],
    hist: Array.from({ length: HIST_LEN }, () => ({ v: 0, throttled: false })),
    paused: false,
    waiting_sign_in: false,
    throttled_until: 0,
    pending_throttle: false,
    last_progress: 0,
  };
}

const MAX_ROWS = 300;

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
  /** Host of the sign-in window that is open right now, if any. */
  signInOpen = $state<string | null>(null);
  ready = $state(false);

  private toastTimer: ReturnType<typeof setTimeout> | undefined;
  private rowIndex = new Map<number, Map<string, FileRow>>();

  /** The job shown in the Transfers view: the running one, else the first waiting one. */
  get active(): QueueItem | undefined {
    const order: QueueItem["status"][] = ["running", "needs_sign_in", "preparing", "paused"];
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

  private lanesCount() {
    return this.settings?.parallel_files ?? 4;
  }

  private liveFor(id: number): LiveJob {
    if (!this.live[id]) this.live[id] = emptyLive(this.lanesCount());
    return this.live[id];
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
          if (gone) index.delete(gone.rel_path);
        }
      }
    };
    switch (ev.type) {
      case "planned":
        // Files that were already up to date arrive as a count here, not as per-file events.
        job.files_total = ev.files + ev.skipped;
        job.skipped = ev.skipped;
        job.bytes_total = ev.bytes;
        break;
      case "file_started":
        if (job.lanes.length <= ev.lane) {
          while (job.lanes.length <= ev.lane) job.lanes.push({ rel_path: null, offset: 0, size: 0 });
        }
        job.lanes[ev.lane] = { rel_path: ev.rel_path, offset: 0, size: ev.size };
        touch({ rel_path: ev.rel_path, size: ev.size, state: "running", check: "" });
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
        for (const l of job.lanes) if (l.rel_path === ev.rel_path) l.rel_path = null;
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
      job.hist.push({ v, throttled });
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
      for (const k of Object.keys(this.live)) if (!ids.has(Number(k))) delete this.live[Number(k)];
    });
    await on<JobEventPayload>("job-event", (p) => this.handleEvent(p.job_id, p.event));
    await on<null>("sessions-changed", () => void this.refreshSessions());
    await on<{ host: string; state: string }>("signin-state", (p) => {
      this.signInOpen = p.state === "window_open" ? p.host : null;
    });
    await on<null>("close-requested", () => (this.closePrompt = true));
    await on<UpdateInfo | null>("update-available", (u) => (this.update = u));

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
