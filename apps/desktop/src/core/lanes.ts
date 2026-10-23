// Parallel lanes (F4): the header text, how many rows to show, and the plain-words reasons the
// throughput chart's markers carry.
import type { LanesReason } from "./types";

export const MAX_LANES = 16;

export interface LaneMark {
  lanes: number;
  reason: LanesReason;
}

function lanesWord(n: number): string {
  return n === 1 ? "1 lane" : `${n} lanes`;
}

/** "Auto · 7 lanes", or "7 lanes" when the user fixed the number. */
export function lanesHeader(auto: boolean, lanes: number): string {
  return auto ? `Auto · ${lanesWord(lanes)}` : lanesWord(lanes);
}

/**
 * How many lane rows to draw: the current count, but never fewer than the highest slot still
 * busy (a lane that was taken away finishes its file first), at least 1 and at most 16.
 */
export function laneRows(target: number, busy: readonly boolean[]): number {
  let top = 0;
  busy.forEach((b, i) => {
    if (b) top = i + 1;
  });
  return Math.min(MAX_LANES, Math.max(1, target, top));
}

export interface MarkText {
  title: string;
  why: string;
}

export function markText(m: LaneMark): MarkText {
  const n = lanesWord(m.lanes);
  switch (m.reason) {
    case "probe_up":
      return { title: `Added a lane: now ${n}`, why: "Speed was still rising, so AmpSync tried one more." };
    case "no_gain":
      return { title: `Back to ${n}`, why: "The extra lane did not make it faster, so AmpSync took it back and holds for a minute." };
    case "throttled":
      return { title: `Cut to ${n}`, why: "SharePoint asked AmpSync to slow down, so it halved the lanes and waits before adding more." };
    case "budget":
      return { title: `Holding at ${n}`, why: "Close to SharePoint's request limit, so AmpSync stops adding lanes and fetches bigger pieces." };
  }
}

/** The label drawn next to a marker: the lane count the change led to. */
export function markLabel(m: LaneMark): string {
  return String(m.lanes);
}
