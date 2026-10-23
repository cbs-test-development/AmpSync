// Shapes that cross the Rust <-> JavaScript boundary. Field names are snake_case on both sides.

export type CloseBehavior = "ask" | "tray" | "pause";
export type SplashMode = "on" | "reduced" | "off";

export interface Settings {
  /** Simultaneous downloads when `auto_tune` is off. */
  parallel_files: number;
  /** AmpSync chooses the number of simultaneous downloads (D22). */
  auto_tune: boolean;
  /** Notify when a download finishes, fails or needs sign-in; absent until the user was asked (D23). */
  notify_on_finish?: boolean | null;
  /** Startup animation (D25). */
  splash: SplashMode;
  range_size: number;
  large_file_threshold: number;
  bandwidth_limit?: number | null;
  max_retries: number;
  tour_seen: boolean;
  save_passwords: boolean;
  check_updates: boolean;
  use_windows_account: boolean;
  default_dest?: string | null;
  close_behavior: CloseBehavior;
}

export type ItemStatus =
  | "queued"
  | "preparing"
  | "running"
  | "paused"
  | "needs_sign_in"
  | "done"
  | "failed"
  | "cancelled";

export interface JobSpec {
  source: string;
  dest: string;
  include: string[];
  exclude: string[];
  files?: string[] | null;
  existing: "skip_unchanged" | "redownload";
  prune: boolean;
}

export interface SummaryLite {
  downloaded: number;
  skipped: number;
  failed: number;
  bytes: number;
  elapsed_ms: number;
  failures: { rel_path: string; error: string }[];
}

export interface QueueItem {
  id: number;
  title: string;
  path_label: string;
  host: string;
  spec: JobSpec;
  est_files: number;
  est_bytes: number;
  status: ItemStatus;
  error?: string | null;
  summary?: SummaryLite | null;
}

export type EngineEvent =
  | { type: "planned"; files: number; bytes: number; skipped: number }
  | { type: "file_started"; rel_path: string; size: number; lane: number }
  | { type: "progress"; bytes_done: number; bytes_total: number; rate_bps: number }
  | { type: "lane_progress"; lane: number; rel_path: string; offset: number; size: number }
  | { type: "file_verified"; rel_path: string; by: "hash" | "size_and_etag" | "size_and_mtime" }
  | { type: "file_failed"; rel_path: string; error: string; will_retry: boolean }
  | { type: "throttled"; retry_after_ms: number }
  | { type: "auth_expired"; host: string }
  | { type: "paused" }
  | { type: "resumed" }
  | ({ type: "finished" } & SummaryLiteEvent)
  | { type: "failed"; error: string }
  | { type: "cancelled" };

export interface SummaryLiteEvent {
  downloaded: number;
  skipped: number;
  failed: number;
  bytes: number;
  elapsed_ms: number;
}

export interface JobEventPayload {
  job_id: number;
  event: EngineEvent;
}

export interface Resolved {
  host: string;
  site_name: string;
  library: string;
  display_name: string;
  server_relative_path: string;
  site_url: string;
}

/**
 * The listing as flat, parallel arrays in display order (folders first, then files, each sorted
 * case-insensitively). A parent always precedes its children and a folder's subtree is the
 * contiguous range after it. Top-level entries have parent -1.
 */
export interface PlanTree {
  total_files: number;
  total_bytes: number;
  listed_ms: number;
  names: string[];
  parents: number[];
  /** 1 for a folder, 0 for a file. */
  dirs: number[];
  /** File sizes in bytes; 0 for folders. */
  sizes: number[];
  /** Last-modified time of each file in unix seconds; 0 for folders. */
  modified: number[];
}

/** What `parse_link` makes of pasted, dropped or clipboard text. */
export interface ParsedLinkInfo {
  host: string;
  kind: "sharing" | "folder";
  /** The text for the link field: trimmed, Safe Links wrapper removed. */
  link: string;
}

/** Payload of the `incoming-link` event (the tray's "Download link from clipboard"). */
export interface IncomingLink {
  text: string;
  source: "tray";
}

export interface SessionInfo {
  host: string;
  method: "webview" | "edge" | "chrome" | "paste";
  captured_at: string;
  status: "active" | "expired" | "checking" | "unknown";
}

export interface AppInfo {
  version: string;
  repo: string;
  build: string;
  target: string;
  credential_store: string;
  os: string;
}

export interface Contributor {
  login: string;
  name: string;
  avatar: string | null;
  contributions: number;
  url: string;
}

export interface ContributorList {
  repo: string;
  items: Contributor[];
}

export interface UpdateInfo {
  version: string;
  url: string;
}

export interface CmdError {
  kind: string;
  message: string;
  host?: string;
}

export interface EnqueueReq {
  source: string;
  save_to: string;
  folder_name: string;
  include: string[];
  exclude: string[];
  /** Exactly these files (D20), instead of globs, for selections too scattered for 50 patterns. */
  files?: string[] | null;
  existing: "skip_unchanged" | "redownload";
  title: string;
  path_label: string;
  est_files: number;
  est_bytes: number;
}

export type ViewId = "transfers" | "new" | "signins" | "settings" | "about";
