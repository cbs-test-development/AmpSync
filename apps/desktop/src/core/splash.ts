// Rules of the startup splash (F7, D25), kept free of the DOM so they can be tested.

/** Dispatched on `window` by App.svelte once the app has loaded its state. */
export const READY_EVENT = "ampsync-ready";

export type SplashMode = "on" | "reduced" | "off";
/** How it actually plays: the full animation, the calm version, or not at all. */
export type Playback = "full" | "calm" | "none";

export interface CreditPerson {
  login: string;
  avatar: string | null;
}

export interface Credit {
  top: string;
  others: number;
  people: CreditPerson[];
}

/** What the backend knows about the start: see `splash_info` in src-tauri/src/splash.rs. */
export interface SplashInfo {
  mode: SplashMode;
  version: string;
  sites: number;
  queued: number;
  paused: number;
  checked: boolean;
  credit: Credit | null;
}

/** The splash stays at least this long, from the moment the window is shown ... */
export const MIN_MS = 3000;
/** ... and never longer than this, whatever is still pending. */
export const MAX_MS = 6000;
/** The status line moves on to these steps no sooner than these times (full animation only). */
export const CHECK_AT_MS = 1900;
export const QUEUE_AT_MS = 2550;

/** The OS "reduce motion" preference makes On behave like Reduced motion. */
export function playback(mode: SplashMode, osReducedMotion: boolean): Playback {
  if (mode === "off") return "none";
  if (mode === "reduced") return "calm";
  return osReducedMotion ? "calm" : "full";
}

/**
 * Whether the splash leaves now. After the minimum it goes as soon as the page is ready and the
 * launch-time sign-in check has answered; at the cap it goes regardless.
 */
export function shouldLeave(elapsedMs: number, frontendReady: boolean, signInsAnswered: boolean): boolean {
  if (elapsedMs >= MAX_MS) return true;
  return elapsedMs >= MIN_MS && frontendReady && signInsAnswered;
}

function plural(n: number, one: string, many: string): string {
  return `${n} ${n === 1 ? one : many}`;
}

export function checkText(sites: number): string {
  return sites > 0 ? `checking sign-ins · ${plural(sites, "site", "sites")}` : "no saved sign-ins";
}

export function queueText(queued: number, paused: number): string {
  const parts: string[] = [];
  if (queued > 0) parts.push(`${plural(queued, "download", "downloads")} queued`);
  if (paused > 0) parts.push(`${paused} paused`);
  return parts.length > 0 ? parts.join(" · ") : "ready";
}

/**
 * The status line at `elapsedMs`: starting, then the sign-in check, then the queue. The queue
 * step shows only once the check has really answered, so the line never claims more than is true.
 */
export function statusAt(elapsedMs: number, info: SplashInfo, answered: boolean, calm: boolean): string {
  const [checkAt, queueAt] = calm ? [0, 0] : [CHECK_AT_MS, QUEUE_AT_MS];
  if (elapsedMs >= queueAt && answered) return queueText(info.queued, info.paused);
  if (elapsedMs >= checkAt) return checkText(info.sites);
  return "starting";
}

/** "built by @login and N contributors"; `null` when there is nobody to name. */
export function creditParts(credit: Credit | null): { name: string; others: string } | null {
  if (!credit || !credit.top) return null;
  const others = credit.others > 0 ? ` and ${plural(credit.others, "contributor", "contributors")}` : "";
  return { name: `@${credit.top}`, others };
}

/** Two letters for a face without a picture. */
export function faceInitials(login: string): string {
  const letters = login.replace(/[^A-Za-z0-9]/g, "");
  return (letters.slice(0, 2) || "?").toUpperCase();
}

/** Keys that are only part of a shortcut and must not dismiss the splash. */
export function skipsSplash(key: string): boolean {
  return !["Tab", "Shift", "Control", "Alt", "Meta", "AltGraph", "CapsLock", "NumLock", "ScrollLock", "Fn", "OS"].includes(key);
}
