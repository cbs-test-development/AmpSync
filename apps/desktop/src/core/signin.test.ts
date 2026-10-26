import { describe, expect, it } from "vitest";
import { defaultFromValue, defaultValue, methodOf, waitText } from "./signin";

describe("sign-in helpers", () => {
  it("maps a remembered choice to the backend method name", () => {
    expect(methodOf("window")).toBe("webview");
    expect(methodOf("edge")).toBe("edge");
    expect(methodOf("chrome")).toBe("chrome");
  });

  it("treats no value as ask-every-time and round-trips the others", () => {
    expect(defaultValue(null)).toBe("ask");
    expect(defaultValue(undefined)).toBe("ask");
    expect(defaultFromValue("ask")).toBeNull();
    for (const c of ["window", "edge", "chrome"] as const) expect(defaultFromValue(defaultValue(c))).toBe(c);
    expect(defaultFromValue("bogus")).toBeNull();
  });

  it("words the wait for each phase and names the host", () => {
    for (const p of ["choosing", "window_open", "browser_open"] as const) {
      expect(waitText(p, "a.example.com").body).toContain("a.example.com");
    }
    expect(waitText("browser_open", "h").title).toMatch(/browser/i);
    expect(waitText("window_open", "h").title).toMatch(/window/i);
  });
});
