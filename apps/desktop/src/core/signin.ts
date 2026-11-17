// Pure helpers for the sign-in dialog and the waiting text (D26).
import type { LinkBrowser, SessionInfo, SignInChoice } from "./types";

/** What the backend is doing for an interactive sign-in (the `signin-state` event, minus "closed"). */
export type SignInPhase = "choosing" | "window_open" | "browser_open" | "link_open";

/** The method name the backend commands take for a remembered choice. */
export function methodOf(choice: SignInChoice): "webview" | "edge" | "chrome" | "browser_link" {
  return choice === "window" ? "webview" : choice;
}

/** "Edge" or "Chrome" for a browser; "your browser" while the link has not said which. */
export function browserName(b: LinkBrowser | null | undefined, fallback = "your browser"): string {
  return b === "edge" ? "Edge" : b === "chrome" ? "Chrome" : fallback;
}

/** How a stored session was made, as the Sign-ins table words it. */
export const METHOD_NAMES: Record<SessionInfo["method"], string> = {
  webview: "AmpSync window",
  edge: "Edge (separate profile)",
  chrome: "Chrome (separate profile)",
  paste: "Pasted session",
  browser_link_edge: "Edge (browser link)",
  browser_link_chrome: "Chrome (browser link)",
};

/** Whether a session came through the browser link (it renews by itself). */
export function isLinked(method: SessionInfo["method"]): boolean {
  return method === "browser_link_edge" || method === "browser_link_chrome";
}

/** Options of the "When AmpSync needs you to sign in" setting; "ask" is stored as no value. */
export function signInDefaults(browser: LinkBrowser | null | undefined): { value: SignInChoice | "ask"; label: string }[] {
  return [
    { value: "ask", label: "Ask every time" },
    { value: "browser_link", label: browser ? `Browser link (${browserName(browser)})` : "Browser link" },
    { value: "window", label: "AmpSync window" },
    { value: "edge", label: "Edge (separate profile)" },
    { value: "chrome", label: "Chrome (separate profile)" },
  ];
}

/** The setting as a select value, and back. */
export function defaultValue(v: SignInChoice | null | undefined): SignInChoice | "ask" {
  return v ?? "ask";
}
export function defaultFromValue(v: string): SignInChoice | null {
  return v === "window" || v === "edge" || v === "chrome" || v === "browser_link" ? v : null;
}

/** The status line New download shows while it waits for a sign-in. */
export function waitText(phase: SignInPhase, host: string, browser?: LinkBrowser | null): { title: string; body: string } {
  switch (phase) {
    case "choosing":
      return { title: "Waiting for your choice.", body: `Choose how to sign in to ${host} in the dialog; this page continues by itself afterwards.` };
    case "link_open":
      return { title: `Finish signing in in ${browserName(browser, "your browser")}.`, body: `${browserName(browser, "Your browser")} asked for a Microsoft sign-in for ${host}; this page continues by itself afterwards.` };
    case "browser_open":
      return { title: "Browser sign-in is open.", body: `Sign in to ${host} in the browser window that opened; this page continues by itself afterwards.` };
    default:
      return { title: "Sign-in window is open.", body: `Sign in to ${host} there; this page continues by itself afterwards.` };
  }
}
