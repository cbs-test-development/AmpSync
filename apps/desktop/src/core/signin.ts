// Pure helpers for the sign-in dialog and the waiting text (D26).
import type { SignInChoice } from "./types";

/** What the backend is doing for an interactive sign-in (the `signin-state` event, minus "closed"). */
export type SignInPhase = "choosing" | "window_open" | "browser_open";

/** The method name the backend commands take for a remembered choice. */
export function methodOf(choice: SignInChoice): "webview" | "edge" | "chrome" {
  return choice === "window" ? "webview" : choice;
}

/** Options of the "When AmpSync needs you to sign in" setting; "ask" is stored as no value. */
export const SIGN_IN_DEFAULTS: { value: SignInChoice | "ask"; label: string }[] = [
  { value: "ask", label: "Ask every time" },
  { value: "window", label: "AmpSync window" },
  { value: "edge", label: "Edge" },
  { value: "chrome", label: "Chrome" },
];

/** The setting as a select value, and back. */
export function defaultValue(v: SignInChoice | null | undefined): SignInChoice | "ask" {
  return v ?? "ask";
}
export function defaultFromValue(v: string): SignInChoice | null {
  return v === "window" || v === "edge" || v === "chrome" ? v : null;
}

/** The status line New download shows while it waits for a sign-in. */
export function waitText(phase: SignInPhase, host: string): { title: string; body: string } {
  switch (phase) {
    case "choosing":
      return { title: "Waiting for your choice.", body: `Choose how to sign in to ${host} in the dialog; this page continues by itself afterwards.` };
    case "browser_open":
      return { title: "Browser sign-in is open.", body: `Sign in to ${host} in the browser window that opened; this page continues by itself afterwards.` };
    default:
      return { title: "Sign-in window is open.", body: `Sign in to ${host} there; this page continues by itself afterwards.` };
  }
}
