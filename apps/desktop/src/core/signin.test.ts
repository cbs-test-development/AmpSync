import { describe, expect, it } from "vitest";
import { METHOD_NAMES, browserName, defaultFromValue, defaultValue, isLinked, methodOf, signInDefaults, waitText } from "./signin";

describe("sign-in helpers", () => {
  it("maps a remembered choice to the backend method name", () => {
    expect(methodOf("window")).toBe("webview");
    expect(methodOf("edge")).toBe("edge");
    expect(methodOf("chrome")).toBe("chrome");
    expect(methodOf("browser_link")).toBe("browser_link");
  });

  it("treats no value as ask-every-time and round-trips the others", () => {
    expect(defaultValue(null)).toBe("ask");
    expect(defaultValue(undefined)).toBe("ask");
    expect(defaultFromValue("ask")).toBeNull();
    for (const c of ["window", "edge", "chrome", "browser_link"] as const) expect(defaultFromValue(defaultValue(c))).toBe(c);
    expect(defaultFromValue("bogus")).toBeNull();
  });

  it("words the wait for each phase and names the host", () => {
    for (const p of ["choosing", "window_open", "browser_open"] as const) {
      expect(waitText(p, "a.example.com").body).toContain("a.example.com");
    }
    expect(waitText("browser_open", "h").title).toMatch(/browser/i);
    expect(waitText("window_open", "h").title).toMatch(/window/i);
  });

  it("words the wait for the browser link and names the browser", () => {
    expect(waitText("link_open", "h", "edge").title).toBe("Finish signing in in Edge.");
    expect(waitText("link_open", "h", "chrome").title).toBe("Finish signing in in Chrome.");
    expect(waitText("link_open", "a.example.com", "edge").body).toContain("a.example.com");
  });

  it("names the browser link methods and recognises them", () => {
    expect(METHOD_NAMES.browser_link_edge).toBe("Edge (browser link)");
    expect(METHOD_NAMES.browser_link_chrome).toBe("Chrome (browser link)");
    expect(isLinked("browser_link_edge")).toBe(true);
    expect(isLinked("edge")).toBe(false);
    expect(browserName(null)).toBe("your browser");
  });

  it("labels the browser link option after the linked browser", () => {
    const label = (b: "edge" | "chrome" | null) => signInDefaults(b).find((o) => o.value === "browser_link")?.label;
    expect(label("edge")).toBe("Browser link (Edge)");
    expect(label("chrome")).toBe("Browser link (Chrome)");
    expect(label(null)).toBe("Browser link");
    expect(signInDefaults(null)[0].value).toBe("ask");
  });
});
