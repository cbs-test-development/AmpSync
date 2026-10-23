import { describe, expect, it } from "vitest";
import { checkSpace, fullText, MARGIN, needsText, notEnoughText, volumeText } from "./diskspace";
import { sizeBrief } from "./format";

const MB = 1024 * 1024;
const GB = 1024 * MB;

describe("short sizes", () => {
  it("shows one decimal under 100 and none above", () => {
    expect(sizeBrief(31.2 * GB)).toBe("31.2 GB");
    expect(sizeBrief(112 * GB)).toBe("112 GB");
    expect(sizeBrief(6.3 * GB)).toBe("6.3 GB");
    expect(sizeBrief(850 * MB)).toBe("850 MB");
    expect(sizeBrief(1.5 * 1024 * GB)).toBe("1.5 TB");
    expect(sizeBrief(0)).toBe("0 B");
    expect(sizeBrief(900)).toBe("900 B");
  });

  it("does not print 1,024 of a unit", () => {
    expect(sizeBrief(1023.99 * MB)).toBe("1 GB");
  });
});

describe("volume names in sentences", () => {
  it("keeps drive letters and shares, names the root volume, and has a fallback", () => {
    expect(volumeText("D:")).toBe("D:");
    expect(volumeText("/home")).toBe("/home");
    expect(volumeText("/")).toBe("the root volume (/)");
    expect(volumeText("")).toBe("this disk");
  });
});

describe("is there room", () => {
  it("fits with the margin to spare", () => {
    expect(checkSpace(10 * GB, 12 * GB)).toEqual({ ok: true, shortBy: 0 });
    // Exactly needs + margin is still fine.
    expect(checkSpace(10 * GB, 10 * GB + MARGIN).ok).toBe(true);
  });

  it("is short when the margin would be eaten, and by how much", () => {
    const c = checkSpace(10 * GB, 10 * GB);
    expect(c.ok).toBe(false);
    expect(c.shortBy).toBe(MARGIN);
    expect(checkSpace(31.2 * GB, 26 * GB).shortBy).toBeCloseTo(6.2 * GB, -6);
  });

  it("treats an empty download as fitting any disk with a gigabyte free", () => {
    expect(checkSpace(0, 2 * GB).ok).toBe(true);
    expect(checkSpace(0, 0.5 * GB).ok).toBe(false);
  });
});

describe("wording", () => {
  it("writes the footer figure", () => {
    expect(needsText(31.2 * GB, 112 * GB, "D:")).toBe("Needs 31.2 GB · 112 GB free on D:");
    expect(needsText(31.2 * GB, null, "")).toBe("Needs 31.2 GB");
  });

  it("writes the refusal with a figure that is never too small", () => {
    expect(notEnoughText(6.3 * GB, "D:")).toBe("Not enough space on D:. Free up 6.3 GB or choose another folder.");
    // 6.21 GB missing must not be shown as 6.2 GB: freeing that much would not be enough.
    expect(notEnoughText(6.21 * GB, "D:")).toBe("Not enough space on D:. Free up 6.3 GB or choose another folder.");
    expect(notEnoughText(40 * MB, "/home")).toBe("Not enough space on /home. Free up 40 MB or choose another folder.");
  });

  it("writes the disk-full banner and notices when room came back", () => {
    expect(fullText("D:", 20 * GB, 13.7 * GB)).toBe("D: is full: 6.3 GB more needed.");
    expect(fullText("D:", 20 * GB, 25 * GB)).toBe("There is room on D: again.");
  });
});
