// Disk space wording and the "is there room" decision (F3). Pure, so it can be tested.
import { sizeBrief } from "./format";

const GiB = 1024 * 1024 * 1024;

/** Kept free on top of what a download needs: temp files, the job state and the OS itself. */
export const MARGIN = GiB;

/** How a volume reads in a sentence: `D:` and `\\nas\media` as they are, `/` as the root volume. */
export function volumeText(volume: string): string {
  if (!volume) return "this disk";
  if (volume === "/") return "the root volume (/)";
  return volume;
}

export interface SpaceCheck {
  /** Whether the selection fits with the margin to spare. */
  ok: boolean;
  /** Bytes to free to make it fit (0 when it already does). */
  shortBy: number;
}

export function checkSpace(needed: number, free: number): SpaceCheck {
  const shortBy = Math.max(0, needed + MARGIN - free);
  return { ok: shortBy === 0, shortBy };
}

/** "Needs 31.2 GB · 112 GB free on D:" */
export function needsText(needed: number, free: number | null, volume: string): string {
  const needs = `Needs ${sizeBrief(needed)}`;
  return free === null ? needs : `${needs} · ${sizeBrief(free)} free on ${volumeText(volume)}`;
}

/** The sentence that replaces the Add to queue button's reason when there is not enough room. */
export function notEnoughText(shortBy: number, volume: string): string {
  return `Not enough space on ${volumeText(volume)}. Free up ${sizeBrief(roundUp(shortBy))} or choose another folder.`;
}

/** Round up to the next 0.1 GB (or whole MB) so the figure shown is never less than what is missing. */
function roundUp(bytes: number): number {
  // The 1e-6 keeps 6.3 GB from becoming 6.4 GB through floating-point noise.
  if (bytes >= GiB) return (Math.ceil((bytes / GiB) * 10 - 1e-6) / 10) * GiB;
  const MiB = 1024 * 1024;
  return Math.ceil(bytes / MiB - 1e-6) * MiB;
}

/** The banner while a running job waits for space: "D: is full: 6.3 GB more needed". */
export function fullText(volume: string, needed: number, freeNow: number): string {
  const more = Math.max(0, needed - freeNow);
  const name = volumeText(volume);
  if (more === 0) return `There is room on ${name} again.`;
  return `${name} is full: ${sizeBrief(roundUp(more))} more needed.`;
}
