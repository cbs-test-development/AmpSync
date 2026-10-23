import { describe, expect, it } from "vitest";
import {
  MAX_MS,
  MIN_MS,
  creditParts,
  faceInitials,
  playback,
  queueText,
  shouldLeave,
  skipsSplash,
  statusAt,
  type SplashInfo,
} from "./splash";

const info = (over: Partial<SplashInfo> = {}): SplashInfo => ({
  mode: "on",
  version: "1.1.0",
  sites: 2,
  queued: 2,
  paused: 0,
  checked: false,
  credit: null,
  ...over,
});

describe("playback", () => {
  it("follows the setting", () => {
    expect(playback("on", false)).toBe("full");
    expect(playback("reduced", false)).toBe("calm");
    expect(playback("off", false)).toBe("none");
  });

  it("lets the OS reduce-motion preference calm the full animation, and nothing else", () => {
    expect(playback("on", true)).toBe("calm");
    expect(playback("reduced", true)).toBe("calm");
    expect(playback("off", true)).toBe("none");
  });
});

describe("shouldLeave", () => {
  it("never before the minimum, even when everything is ready", () => {
    expect(shouldLeave(0, true, true)).toBe(false);
    expect(shouldLeave(MIN_MS - 1, true, true)).toBe(false);
  });

  it("leaves at the minimum once the page is ready and the sign-in check answered", () => {
    expect(shouldLeave(MIN_MS, true, true)).toBe(true);
    expect(shouldLeave(MIN_MS + 500, true, true)).toBe(true);
  });

  it("keeps waiting after the minimum while either is missing", () => {
    expect(shouldLeave(MIN_MS, false, true)).toBe(false);
    expect(shouldLeave(MIN_MS, true, false)).toBe(false);
    expect(shouldLeave(MAX_MS - 1, false, false)).toBe(false);
  });

  it("leaves at the cap regardless", () => {
    expect(shouldLeave(MAX_MS, false, false)).toBe(true);
    expect(shouldLeave(MAX_MS + 1000, false, true)).toBe(true);
  });
});

describe("statusAt", () => {
  it("walks through the real steps, never claiming the check is done before it is", () => {
    expect(statusAt(0, info(), false, false)).toBe("starting");
    expect(statusAt(1900, info(), false, false)).toBe("checking sign-ins · 2 sites");
    expect(statusAt(2600, info(), false, false)).toBe("checking sign-ins · 2 sites");
    expect(statusAt(2600, info(), true, false)).toBe("2 downloads queued");
    expect(statusAt(5000, info(), false, false)).toBe("checking sign-ins · 2 sites");
  });

  it("does not jump to the queue before its slot in the full animation", () => {
    expect(statusAt(2000, info(), true, false)).toBe("checking sign-ins · 2 sites");
  });

  it("is live from the start in the calm version", () => {
    expect(statusAt(0, info(), false, true)).toBe("checking sign-ins · 2 sites");
    expect(statusAt(0, info(), true, true)).toBe("2 downloads queued");
  });

  it("says so when there is nothing to check", () => {
    expect(statusAt(2000, info({ sites: 0 }), false, false)).toBe("no saved sign-ins");
    expect(statusAt(2000, info({ sites: 1 }), false, false)).toBe("checking sign-ins · 1 site");
  });
});

describe("queueText", () => {
  it("describes the queue", () => {
    expect(queueText(0, 0)).toBe("ready");
    expect(queueText(1, 0)).toBe("1 download queued");
    expect(queueText(3, 0)).toBe("3 downloads queued");
    expect(queueText(0, 2)).toBe("2 paused");
    expect(queueText(2, 1)).toBe("2 downloads queued · 1 paused");
  });
});

describe("creditParts", () => {
  const person = { login: "x", avatar: null };
  it("names the top contributor and counts the others", () => {
    expect(creditParts({ top: "ann", others: 3, people: [person] })).toEqual({ name: "@ann", others: " and 3 contributors" });
    expect(creditParts({ top: "ann", others: 1, people: [person] })).toEqual({ name: "@ann", others: " and 1 contributor" });
  });

  it("names only one person when there is no one else", () => {
    expect(creditParts({ top: "ann", others: 0, people: [person] })).toEqual({ name: "@ann", others: "" });
  });

  it("is hidden without a list", () => {
    expect(creditParts(null)).toBeNull();
    expect(creditParts({ top: "", others: 0, people: [] })).toBeNull();
  });
});

describe("faceInitials and skipsSplash", () => {
  it("takes two letters from a login", () => {
    expect(faceInitials("octocat")).toBe("OC");
    expect(faceInitials("-a")).toBe("A");
    expect(faceInitials("--")).toBe("?");
  });

  it("any real key skips, shortcut keys alone do not", () => {
    expect(skipsSplash("Enter")).toBe(true);
    expect(skipsSplash(" ")).toBe(true);
    expect(skipsSplash("Escape")).toBe(true);
    expect(skipsSplash("a")).toBe(true);
    expect(skipsSplash("Tab")).toBe(false);
    expect(skipsSplash("Shift")).toBe(false);
    expect(skipsSplash("Control")).toBe(false);
  });
});
