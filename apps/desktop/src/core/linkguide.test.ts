import { describe, expect, it } from "vitest";
import {
  GUIDE_STEPS,
  LAST_STEP,
  developerModeWhere,
  extensionsUrl,
  initialBrowser,
  nudgeBody,
  nudgeHeadline,
  ordinal,
  primaryEnabled,
  primaryLabel,
  renewalsText,
  revealLabel,
  stepMark,
  stepState,
} from "./linkguide";

describe("nudge text", () => {
  it("writes the ordinal of the prompt count correctly", () => {
    const cases: [number, string][] = [[1, "1st"], [2, "2nd"], [3, "3rd"], [4, "4th"], [11, "11th"], [12, "12th"], [13, "13th"], [21, "21st"], [22, "22nd"], [101, "101st"], [111, "111th"]];
    for (const [n, text] of cases) expect(ordinal(n)).toBe(text);
  });

  it("names the count in the headline and never says less than 2nd", () => {
    expect(nudgeHeadline(3)).toBe("That's the 3rd time this week. Want it to stop?");
    expect(nudgeHeadline(2)).toBe("That's the 2nd time this week. Want it to stop?");
    expect(nudgeHeadline(0)).toContain("2nd");
  });

  it("says the download carries on only where it does", () => {
    expect(nudgeBody("edge", true)).toContain("this download carries on straight away");
    expect(nudgeBody("chrome", false)).not.toContain("carries on");
    expect(nudgeBody("chrome", false)).toContain("Chrome");
    expect(nudgeBody(null, false)).toContain("Edge or Chrome");
  });
});

describe("guide steps", () => {
  it("has four steps and marks done, current and coming ones", () => {
    expect(GUIDE_STEPS).toHaveLength(4);
    expect(LAST_STEP).toBe(3);
    expect([0, 1, 2, 3].map((i) => stepState(i, 2))).toEqual(["done", "done", "now", "todo"]);
    expect([0, 1, 2, 3].map((i) => stepMark(i, 2))).toEqual(["✓", "✓", "3", "4"]);
  });

  it("starts on the linked browser, else Edge", () => {
    expect(initialBrowser(null)).toBe("edge");
    expect(initialBrowser({ browser: null })).toBe("edge");
    expect(initialBrowser({ browser: "chrome" })).toBe("chrome");
  });

  it("words the primary button per step and waits for the connection at the end", () => {
    expect(primaryLabel(0, "edge", false)).toBe("Open Edge extensions");
    expect(primaryLabel(0, "chrome", false)).toBe("Open Chrome extensions");
    expect(primaryLabel(1, "edge", false)).toBe("I've turned it on");
    expect(primaryLabel(2, "edge", false)).toBe("I've added it");
    expect(primaryLabel(3, "edge", false)).toBe("Waiting…");
    expect(primaryLabel(3, "edge", true)).toBe("Finish");
    expect(primaryEnabled(2, false)).toBe(true);
    expect(primaryEnabled(3, false)).toBe(false);
    expect(primaryEnabled(3, true)).toBe(true);
  });

  it("gives the per-browser addresses and hints", () => {
    expect(extensionsUrl("edge")).toBe("edge://extensions");
    expect(extensionsUrl("chrome")).toBe("chrome://extensions");
    expect(developerModeWhere("edge")).toMatch(/left-hand/);
    expect(developerModeWhere("chrome")).toMatch(/top right/);
  });

  it("calls the reveal button after the file manager of the system", () => {
    expect(revealLabel("windows")).toBe("Show in Explorer");
    expect(revealLabel("macos")).toBe("Show in Finder");
    expect(revealLabel("linux")).toBe("Show in folder");
    expect(revealLabel(undefined)).toBe("Show in folder");
  });

  it("words the renewals line", () => {
    expect(renewalsText(11, 0)).toBe("11 · you were asked 0 times");
    expect(renewalsText(2, 1)).toBe("2 · you were asked 1 time");
  });
});
