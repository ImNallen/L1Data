import { describe, expect, it } from "vitest";
import { createScanner } from "jsonc-parser";
import {
  dayText,
  draftText,
  editOutcome,
  formatJson,
  freshTemporal,
  jsonError,
  mondayIndex,
  moveDay,
  moveMonth,
  parseDay,
  parseTemporal,
  temporalError,
  temporalText,
  spliceSource,
  textareaChange,
} from "./gridEditorValue";
import type { TemporalShape } from "./gridEditorType";

const zoned: TemporalShape = { kind: "timestamp", zone: "offset" };

describe("exact temporal assistance", () => {
  it.each(["2024-02-29 12:34:56.123456+01", "2026-11-18T14:30:00.1234567-03:30", "0001-01-01 00:00:00.0000000Z", "2026-01-01 01:02:03+05:30:20"])(
    "retains every source slice of %s",
    (text) => {
      const parts = parseTemporal(text, zoned);
      expect(parts).not.toBeNull();
      expect(temporalText(parts!)).toBe(text);
      expect(temporalText({ ...parts!, date: "2026-12-25" })).toBe("2026-12-25" + text.slice(10));
    },
  );
  it.each([
    "infinity",
    "-infinity",
    "0001-01-01 00:00:00+00 BC",
    "18/11/2026 14:30:00+01",
    "2026-02-30 00:00:00+00",
    "2026-01-01 24:00:00+00",
    "2026-01-01 00:00:00.12345678+00",
    "2026-01-01 00:00:00",
  ])("leaves unsupported %s in exact mode", (text) => {
    expect(parseTemporal(text, zoned)).toBeNull();
    expect(draftText({ kind: "raw", text, edited: false })).toBe(text);
  });
  it("keeps date-only, time-only and unzoned shapes separate", () => {
    expect(parseTemporal("2024-02-29", { kind: "date" })).not.toBeNull();
    expect(parseTemporal("12:34:56.1234567", { kind: "time", zone: "none" })).not.toBeNull();
    expect(parseTemporal("12:34:56.123456+01:00", { kind: "time", zone: "offset" })).not.toBeNull();
    expect(parseTemporal("2026-01-01 12:00:00+01", { kind: "timestamp", zone: "none" })).toBeNull();
    expect(parseTemporal("2026-01-01 12:00:00", { kind: "time", zone: "none" })).toBeNull();
  });
  it("validates lexical fields without discarding incomplete text", () => {
    const parts = parseTemporal("2026-11-18 14:30:00.1234567+01:00", zoned)!;
    for (const change of [{ time: "14:" }, { fraction: "12x" }, { offset: "+0" }]) {
      const incomplete = { ...parts, ...change };
      expect(temporalError(incomplete, zoned)).toBeTypeOf("string");
      expect(draftText({ kind: "parts", parts: incomplete, edited: true })).toBe(temporalText(incomplete));
    }
    expect(temporalError(parts, zoned)).toBeNull();
  });
  it("handles leap years, year 1, Monday starts, month/year jumps and clamping", () => {
    expect(parseDay("0000-01-01")).toBeNull();
    expect(parseDay("1900-02-29")).toBeNull();
    expect(parseDay("2000-02-29")).not.toBeNull();
    expect(mondayIndex({ year: 1, month: 1, day: 1 })).toBe(0);
    expect(moveMonth({ year: 2024, month: 1, day: 31 }, 1)).toEqual({ year: 2024, month: 2, day: 29 });
    expect(moveMonth({ year: 2024, month: 2, day: 29 }, 12)).toEqual({ year: 2025, month: 2, day: 28 });
    expect(dayText(moveDay({ year: 1, month: 12, day: 31 }, 1))).toBe("0002-01-01");
    const now = new Date(0);
    now.setFullYear(1, 0, 1);
    now.setHours(2, 3, 4, 0);
    expect(freshTemporal({ kind: "date" }, now).date).toBe("0001-01-01");
  });
});

it("distinguishes untouched NULL/DEFAULT, deliberately empty text and actual unchanged text", () => {
  for (const kind of ["null", "default"] as const) {
    expect(editOutcome({ kind }, { kind: "raw", text: "", edited: false })).toEqual({ kind: "unchanged" });
    expect(editOutcome({ kind }, { kind: "raw", text: "", edited: true })).toEqual({ kind: "value", value: "" });
  }
  expect(editOutcome({ kind: "text", text: "" }, { kind: "raw", text: "", edited: true })).toEqual({ kind: "unchanged" });
  const huge = "90071992547409931234567890.000000000000000001";
  expect(editOutcome({ kind: "null" }, { kind: "raw", text: huge, edited: true })).toEqual({ kind: "value", value: huge });
});

it("formats JSON whitespace without changing huge numbers, duplicate keys, escapes or order", () => {
  const source = '{"n":900719925474099312345678901234567890,"n":1e+999,"s":"\\u0061\\/","a":[true,null]}';
  const tokens = (text: string) => {
    const scanner = createScanner(text, true),
      result: string[] = [];
    for (;;) {
      scanner.scan();
      if (!scanner.getTokenLength()) break;
      result.push(text.slice(scanner.getTokenOffset(), scanner.getTokenOffset() + scanner.getTokenLength()));
    }
    return result;
  };
  expect(jsonError(source)).toBeNull();
  const formatted = formatJson(source);
  expect(formatted).toContain('"n": 900719925474099312345678901234567890');
  expect(formatted).toContain('"n": 1e+999');
  expect(formatted).toContain('"s": "\\u0061\\/"');
  expect(tokens(formatted)).toEqual(tokens(source));
  expect(formatted).toContain("\n");
  for (const invalid of ["", "{", '{"a":1,}', '{/*comment*/"a":1}', "NaN"]) expect(jsonError(invalid)).toBeTypeOf("string");
});

it("preserves source CRLF/CR and maps textarea selections for typing and paste", () => {
  const source = "one\r\ntwo\rthree";
  expect(textareaChange(source, "one\ntwox\nthree")).toBe("one\r\ntwox\rthree");
  expect(textareaChange(source, "one\ntwo\nthree")).toBe(source);
  expect(spliceSource(source, 4, 7, "X\r\nY")).toBe("one\r\nX\r\nY\rthree");
});
