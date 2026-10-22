import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  AppInfo,
  CmdError,
  ContributorList,
  EnqueueReq,
  PlanTree,
  QueueItem,
  Resolved,
  SessionInfo,
  Settings,
  UpdateInfo,
} from "./types";

/** Backend errors arrive as `{ kind, message }`; anything else becomes a generic error. */
export function toCmdError(e: unknown): CmdError {
  if (e && typeof e === "object" && "kind" in e && "message" in e) return e as CmdError;
  return { kind: "error", message: typeof e === "string" ? e : "Something went wrong." };
}

export const api = {
  resolve: (link: string) => invoke<Resolved>("resolve", { link }),
  plan: (link: string) => invoke<PlanTree>("plan", { link }),
  enqueue: (req: EnqueueReq) => invoke<number>("enqueue", { req }),
  getQueue: () => invoke<QueueItem[]>("get_queue"),
  pause: (job_id: number) => invoke<void>("pause", { job_id }),
  resume: (job_id: number) => invoke<void>("resume", { job_id }),
  cancel: (job_id: number) => invoke<void>("cancel", { job_id }),
  retry: (job_id: number) => invoke<void>("retry", { job_id }),
  clearFinished: () => invoke<void>("clear_finished"),
  sessions: () => invoke<SessionInfo[]>("sessions"),
  checkSessions: () => invoke<void>("check_sessions"),
  signIn: (link: string, method: "webview" | "edge" | "chrome" | "paste", cookie_header?: string) =>
    invoke<SessionInfo>("sign_in", { req: { link, method, cookie_header } }),
  cancelSignIn: () => invoke<void>("cancel_sign_in"),
  signOut: (host: string) => invoke<void>("sign_out", { host }),
  getSettings: () => invoke<Settings>("get_settings"),
  setSettings: (settings: Settings) => invoke<Settings>("set_settings", { settings }),
  markTourSeen: () => invoke<Settings>("mark_tour_seen"),
  appInfo: () => invoke<AppInfo>("app_info"),
  contributors: () => invoke<ContributorList>("contributors"),
  getUpdate: () => invoke<UpdateInfo | null>("get_update"),
  openExternal: (url: string) => invoke<void>("open_external", { url }),
  closeDecision: (choice: "tray" | "pause", remember: boolean) =>
    invoke<void>("close_decision", { decision: { choice, remember } }),
};

export async function on<T>(name: string, fn: (payload: T) => void): Promise<UnlistenFn> {
  return listen<T>(name, (e) => fn(e.payload));
}
