import { describe, expect, it } from "vitest";
import { laneRows, lanesHeader, markText, MAX_LANES } from "./lanes";

describe("lanes header", () => {
  it("says Auto when the engine chooses", () => {
    expect(lanesHeader(true, 7)).toBe("Auto · 7 lanes");
    expect(lanesHeader(false, 7)).toBe("7 lanes");
    expect(lanesHeader(false, 1)).toBe("1 lane");
  });
});

describe("how many lane rows to draw", () => {
  const busy = (...on: number[]) => Array.from({ length: MAX_LANES }, (_, i) => on.includes(i));

  it("follows the lane count", () => {
    expect(laneRows(4, busy())).toBe(4);
    expect(laneRows(16, busy())).toBe(16);
  });

  it("keeps a removed lane visible until it has finished its file", () => {
    expect(laneRows(5, busy(0, 1, 6))).toBe(7);
    expect(laneRows(5, busy(0, 1))).toBe(5);
  });

  it("stays between 1 and 16", () => {
    expect(laneRows(0, busy())).toBe(1);
    expect(laneRows(40, busy())).toBe(16);
  });
});

describe("what a chart marker says", () => {
  it("explains each reason in plain words", () => {
    expect(markText({ lanes: 7, reason: "probe_up" }).title).toBe("Added a lane: now 7 lanes");
    expect(markText({ lanes: 6, reason: "no_gain" }).title).toBe("Back to 6 lanes");
    expect(markText({ lanes: 4, reason: "throttled" }).why).toMatch(/SharePoint asked AmpSync to slow down/);
    expect(markText({ lanes: 12, reason: "budget" }).title).toBe("Holding at 12 lanes");
  });
});
