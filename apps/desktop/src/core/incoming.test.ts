import { describe, expect, it } from "vitest";
import { carriesFiles, carriesLink, isTextTarget, linkText } from "./incoming";

describe("linkText", () => {
  it("prefers the first real line of a uri-list", () => {
    expect(linkText("# from the address bar\r\nhttps://a.example/x\r\nhttps://b.example/y\r\n", "ignored")).toBe("https://a.example/x");
  });

  it("falls back to the plain text, trimmed", () => {
    expect(linkText("", "  https://a.example/x \n")).toBe("https://a.example/x");
    expect(linkText("# only a comment\n", "text")).toBe("text");
    expect(linkText("", "")).toBe("");
  });
});

describe("drag types", () => {
  it("accepts text and URLs, never files", () => {
    expect(carriesLink(["text/uri-list", "text/plain"])).toBe(true);
    expect(carriesLink(["text/plain"])).toBe(true);
    expect(carriesLink(["Files"])).toBe(false);
    expect(carriesLink(["text/html"])).toBe(false);
    expect(carriesFiles(["Files", "text/plain"])).toBe(true);
    expect(carriesFiles(["text/plain"])).toBe(false);
  });
});

describe("isTextTarget", () => {
  it("leaves paste alone inside text fields", () => {
    expect(isTextTarget({ tagName: "INPUT", type: "text" })).toBe(true);
    expect(isTextTarget({ tagName: "input", type: "search" })).toBe(true);
    expect(isTextTarget({ tagName: "INPUT" })).toBe(true);
    expect(isTextTarget({ tagName: "TEXTAREA" })).toBe(true);
    expect(isTextTarget({ tagName: "SELECT" })).toBe(true);
    expect(isTextTarget({ tagName: "DIV", isContentEditable: true })).toBe(true);
  });

  it("takes paste over everywhere else", () => {
    expect(isTextTarget(null)).toBe(false);
    expect(isTextTarget({ tagName: "BODY" })).toBe(false);
    expect(isTextTarget({ tagName: "BUTTON" })).toBe(false);
    expect(isTextTarget({ tagName: "INPUT", type: "checkbox" })).toBe(false);
    expect(isTextTarget({ tagName: "INPUT", type: "radio" })).toBe(false);
  });
});
