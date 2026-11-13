import { describe, expect, it } from "vitest";
import { formatClock, formatElapsed, groupDigits, hasResultPager, isNumeric, pageRows, pageStatus, resultPage, shortType, toTsv } from "./format";

describe("groupDigits", () => {
  it("groups the integer part and keeps sign and fraction verbatim", () => {
    expect(groupDigits("-1234567.890")).toBe("-1,234,567.890");
    expect(groupDigits("1021447.90")).toBe("1,021,447.90");
    expect(groupDigits("3402")).toBe("3,402");
    expect(groupDigits("742")).toBe("742");
    expect(groupDigits("0.000123")).toBe("0.000123");
  });

  it("never loses precision beyond 2^53", () => {
    expect(groupDigits("9007199254740993")).toBe("9,007,199,254,740,993");
    expect(groupDigits("12345678901234567890.123456789")).toBe("12,345,678,901,234,567,890.123456789");
  });

  it("does not group digits after the decimal point or in the exponent", () => {
    expect(groupDigits("1.23456789")).toBe("1.23456789");
    expect(groupDigits("1234.5e+10000")).toBe("1,234.5e+10000");
  });

  it("passes through text that is not a plain decimal", () => {
    for (const s of ["NaN", "Infinity", "-Infinity", "$1,234.00", "", "1.2.3"]) expect(groupDigits(s)).toBe(s);
  });
});

describe("toTsv", () => {
  it("joins cells with tabs, rows with newlines, and empties NULL", () => {
    expect(
      toTsv([
        ["a", null, "3"],
        ["b", "x", "4"],
      ]),
    ).toBe("a\t\t3\nb\tx\t4");
  });

  it("quotes values that contain tabs, newlines, or quotes", () => {
    expect(toTsv([['say "hi"', "two\nlines", "a\tb"]])).toBe('"say ""hi"""\t"two\nlines"\t"a\tb"');
  });
});

describe("shortType", () => {
  it("uses the short names the grid shows, keeping modifiers and array brackets", () => {
    expect(shortType("timestamp with time zone")).toBe("timestamptz");
    expect(shortType("timestamp(3) with time zone[]")).toBe("timestamptz(3)[]");
    expect(shortType("timestamp without time zone")).toBe("timestamp");
    expect(shortType("character varying(20)")).toBe("varchar(20)");
    expect(shortType("character(3)")).toBe("char(3)");
    expect(shortType("numeric(12,2)")).toBe("numeric(12,2)");
    expect(shortType("lab.mood")).toBe("lab.mood");
  });
});

describe("isNumeric", () => {
  it("treats integers and other numbers alike and nothing else", () => {
    expect((["Integer", "Numeric", "Text", "Bool", "Temporal", "Other"] as const).map(isNumeric)).toEqual([true, true, false, false, false, false]);
  });
});

describe("resultPage", () => {
  it("windows and labels a page of the stored rows", () => {
    expect(resultPage(200_000, 500, 1)).toEqual({
      page: 1,
      lastPage: 399,
      window: { start: 500, length: 500 },
      label: "501–1,000 of 200,000",
      isLast: false,
    });
  });

  it("shortens the last page", () => {
    expect(resultPage(1_205, 500, 2)).toEqual({ page: 2, lastPage: 2, window: { start: 1000, length: 205 }, label: "1,001–1,205 of 1,205", isLast: true });
  });

  it("ends on a full page when the count is an exact multiple of the size", () => {
    expect(resultPage(1_000, 500, 1)).toEqual({ page: 1, lastPage: 1, window: { start: 500, length: 500 }, label: "501–1,000 of 1,000", isLast: true });
  });

  it("clamps a page past the end to the last page", () => {
    expect(resultPage(1_205, 500, 9)).toMatchObject({ page: 2, window: { start: 1000, length: 205 }, isLast: true });
  });

  it("shows a pager only above the smallest page size", () => {
    expect([hasResultPager(10), hasResultPager(11)]).toEqual([false, true]);
  });
});

describe("pageStatus", () => {
  it("uses the row estimate for the total until a short page shows the exact count", () => {
    expect(pageStatus(0, 500, 500, 241183, undefined)).toEqual({ label: "1–500 of ~241,183", isLast: false });
    expect(pageStatus(2, 100, 100, 241183, undefined)).toEqual({ label: "201–300 of ~241,183", isLast: false });
    expect(pageStatus(482, 500, 183, 241183, undefined)).toEqual({ label: "241,001–241,183 of 241,183", isLast: true });
  });

  it("uses a counted total, so a full last page is known to be last", () => {
    expect(pageStatus(481, 500, 500, 241000, 241000)).toEqual({ label: "240,501–241,000 of 241,000", isLast: true });
    expect(pageStatus(0, 500, 500, 241000, 241000)).toEqual({ label: "1–500 of 241,000", isLast: false });
  });

  it("falls back to a lower bound when the estimate is missing or stale", () => {
    expect(pageStatus(0, 500, 500, null, undefined).label).toBe("1–500 of 500+");
    expect(pageStatus(3, 10, 10, 20, undefined).label).toBe("31–40 of 40+");
  });

  it("reports empty pages without a range", () => {
    expect(pageStatus(0, 500, 0, 0, undefined)).toEqual({ label: "0 of 0", isLast: true });
    expect(pageStatus(2, 500, 0, 1000, 1000)).toEqual({ label: "0 of 1,000", isLast: true });
  });
});

describe("formatElapsed", () => {
  it("uses ms under a second and seconds above", () => {
    expect(formatElapsed(38)).toBe("38 ms");
    expect(formatElapsed(1_234)).toBe("1.23 s");
    expect(formatElapsed(75_000)).toBe("1m 15s");
  });
});

describe("formatClock", () => {
  it("prints local time as zero-padded HH:MM:SS", () => {
    expect(formatClock(new Date(2026, 9, 15, 9, 5, 7, 900).getTime())).toBe("09:05:07");
    expect(formatClock(new Date(2026, 9, 15, 23, 59, 59).getTime())).toBe("23:59:59");
    expect(formatClock(new Date(2026, 9, 15, 0, 0, 0).getTime())).toBe("00:00:00");
  });
});

describe("pageRows", () => {
  it("expects the rows left on the page from the table's total", () => {
    expect(pageRows(0, 10, 3)).toBe(3);
    expect(pageRows(0, 10, 18_400)).toBe(10);
    expect(pageRows(1, 10, 15)).toBe(5);
    expect(pageRows(2, 10, 15)).toBe(0);
  });

  it("knows nothing without a total", () => {
    expect(pageRows(0, 10, null)).toBeUndefined();
  });
});
