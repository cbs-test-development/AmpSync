// Shapes that cross the Rust <-> JavaScript boundary. Field names are snake_case on both sides.

export type CloseBehavior = "ask" | "tray" | "pause";

export interface Settings {
  parallel_files: number;
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
  | { type: "file_verified"; rel_path: string; by: "hash" | "size_and_etag" }
  | { type: "file_skipped"; rel_path: string }
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

export interface TreeNode {
  path: string;
  name: string;
  depth: number;
  kind: "folder" | "file";
  files: number;
  bytes: number;
}

export interface PlanTree {
  total_files: number;
  total_bytes: number;
  listed_ms: number;
  nodes: TreeNode[];
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
  existing: "skip_unchanged" | "redownload";
  title: string;
  path_label: string;
  est_files: number;
  est_bytes: number;
}

export type ViewId = "transfers" | "new" | "signins" | "settings" | "about";
