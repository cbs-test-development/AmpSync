import { describe, expect, it } from "vitest";
import { steps } from "./tour";

describe("tour", () => {
  it("has eight steps", () => {
    expect(steps).toHaveLength(8);
  });

  it("offers browser link set-up as the second step, pointing at Sign-ins", () => {
    expect(steps[1].action).toBe("link");
    expect(steps[1].target).toBe('[data-go="signins"]');
    expect(steps[1].title).toBe("Never sign in to AmpSync again");
  });

  it("has an action on no other step", () => {
    expect(steps.filter((s) => s.action).length).toBe(1);
  });
});
