// Pure rules for the browser link set-up guide and the "link my browser" nudge, kept free of the
// DOM so they can be tested.
import { browserName } from "./signin";
import type { LinkBrowser, LinkStatus } from "./types";

export const GUIDE_STEPS = ["Open the extensions page", "Turn on Developer mode", "Add the AmpSync extension", "Connect"] as const;
export const LAST_STEP = GUIDE_STEPS.length - 1;

export type StepState = "done" | "now" | "todo";

export function stepState(index: number, current: number): StepState {
  return index < current ? "done" : index === current ? "now" : "todo";
}

/** The number in a step's badge; a check mark once it is done. */
export function stepMark(index: number, current: number): string {
  return index < current ? "✓" : String(index + 1);
}

/** The browser the guide starts on: the one the link last said hello from, else Edge. */
export function initialBrowser(link: Pick<LinkStatus, "browser"> | null | undefined): LinkBrowser {
  return link?.browser ?? "edge";
}

export function extensionsUrl(b: LinkBrowser): string {
  return `${b}://extensions`;
}

export function developerModeWhere(b: LinkBrowser): string {
  return b === "edge" ? "At the bottom of the left-hand panel, switch on Developer mode." : "At the top right of the page, switch on Developer mode.";
}

export function lede(b: LinkBrowser): string {
  return `AmpSync reuses the SharePoint sign-in your ${browserName(b)} already keeps fresh. Set it up once.`;
}

/** The label of the primary button for a step. */
export function primaryLabel(step: number, b: LinkBrowser, connected: boolean): string {
  switch (step) {
    case 0: return `Open ${browserName(b)} extensions`;
    case 1: return "I've turned it on";
    case 2: return "I've added it";
    default: return connected ? "Finish" : "Waiting…";
  }
}

/** The primary button on the last step does nothing until the extension has connected. */
export function primaryEnabled(step: number, connected: boolean): boolean {
  return step < LAST_STEP || connected;
}

/** Where "Show in folder" is called differently per system. */
export function revealLabel(os: string | undefined): string {
  return os === "windows" ? "Show in Explorer" : os === "macos" ? "Show in Finder" : "Show in folder";
}

/** "1st", "2nd", "3rd", "4th", "11th", "12th", "21st" ... */
export function ordinal(n: number): string {
  const m100 = n % 100;
  if (m100 >= 11 && m100 <= 13) return `${n}th`;
  switch (n % 10) {
    case 1: return `${n}st`;
    case 2: return `${n}nd`;
    case 3: return `${n}rd`;
    default: return `${n}th`;
  }
}

/** The nudge's headline, from the number of sign-in prompts in the last 7 days. */
export function nudgeHeadline(prompts7d: number): string {
  return `That's the ${ordinal(Math.max(prompts7d, 2))} time this week. Want it to stop?`;
}

export function nudgeBody(browser: LinkBrowser | null | undefined, continuesDownload: boolean): string {
  const tail = continuesDownload ? ", and this download carries on straight away" : "";
  return `Link AmpSync to ${browserName(browser, "Edge or Chrome")} and it renews the sign-in by itself${tail}. Set-up takes about 2 minutes.`;
}

/** "11 · you were asked 0 times" in the Sign-ins panel. */
export function renewalsText(renewals7d: number, prompts7d: number): string {
  return `${renewals7d} · you were asked ${prompts7d} ${prompts7d === 1 ? "time" : "times"}`;
}
