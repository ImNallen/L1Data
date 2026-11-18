import { applyEdits, format, parseTree, printParseErrorCode, type ParseError } from "jsonc-parser";
import type { TemporalShape } from "./gridEditorType";

export type InitialValue = { kind: "text"; text: string } | { kind: "null" | "default" };
export type EditOutcome = { kind: "cancel" | "unchanged" } | { kind: "value"; value: string };
export type CalendarDay = { year: number; month: number; day: number };
export type TemporalParts = { date: string; separator: string; time: string; fraction: string; offset: string };
export type EditorDraft = { kind: "raw"; text: string; edited: boolean } | { kind: "parts"; parts: TemporalParts; edited: boolean };

export const pad = (n: number, width = 2) => String(n).padStart(width, "0");
export const dayText = (d: CalendarDay) => `${pad(d.year, 4)}-${pad(d.month)}-${pad(d.day)}`;
export function daysInMonth(year: number, month: number): number {
  return month === 2 ? (year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0) ? 29 : 28) : [4, 6, 9, 11].includes(month) ? 30 : 31;
}
export function parseDay(text: string): CalendarDay | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(text);
  if (!m) return null;
  const [year, month, day] = m.slice(1).map(Number);
  return year! >= 1 && month! >= 1 && month! <= 12 && day! >= 1 && day! <= daysInMonth(year!, month!) ? { year: year!, month: month!, day: day! } : null;
}
export function moveMonth(d: CalendarDay, delta: number): CalendarDay {
  const total = Math.max(12, Math.min(9999 * 12 + 11, d.year * 12 + d.month - 1 + delta));
  const year = Math.floor(total / 12),
    month = (total % 12) + 1;
  return { year, month, day: Math.min(d.day, daysInMonth(year, month)) };
}
function calendarDate(d: CalendarDay): Date {
  const date = new Date(0);
  date.setUTCHours(0, 0, 0, 0);
  date.setUTCFullYear(d.year, d.month - 1, d.day);
  return date;
}
export function moveDay(d: CalendarDay, delta: number): CalendarDay {
  const date = calendarDate(d);
  date.setUTCDate(date.getUTCDate() + delta);
  const year = date.getUTCFullYear();
  return year < 1 || year > 9999 ? d : { year, month: date.getUTCMonth() + 1, day: date.getUTCDate() };
}
export const mondayIndex = (d: CalendarDay) => (calendarDate(d).getUTCDay() + 6) % 7;
export const localDay = (date = new Date()): CalendarDay => ({ year: date.getFullYear(), month: date.getMonth() + 1, day: date.getDate() });
export function temporalText(parts: TemporalParts): string {
  return parts.date + parts.separator + parts.time + (parts.fraction ? `.${parts.fraction}` : "") + parts.offset;
}
export function temporalError(parts: TemporalParts, shape: TemporalShape): string | null {
  if (shape.kind !== "time" && !parseDay(parts.date)) return "Enter a valid date (YYYY-MM-DD).";
  if (shape.kind === "date") return null;
  if (!/^(?:[01]\d|2[0-3]):[0-5]\d(?::[0-5]\d)?$/.test(parts.time)) return "Enter a complete 24-hour time (HH:MM or HH:MM:SS).";
  if (!/^\d*$/.test(parts.fraction) || parts.fraction.length > 7 || (parts.fraction && parts.time.split(":").length !== 3))
    return "Use up to 7 fractional digits after seconds.";
  if (shape.zone === "offset" && !/^(?:Z|[+-](?:0\d|1[0-5])(?::?[0-5]\d)?(?::?[0-5]\d)?)$/.test(parts.offset)) return "Enter a UTC offset, for example +01:00.";
  return null;
}
export function parseTemporal(text: string, shape: TemporalShape): TemporalParts | null {
  if (shape.kind === "date") return parseDay(text) ? { date: text, separator: "", time: "", fraction: "", offset: "" } : null;
  const match = /^(?:(\d{4}-\d{2}-\d{2})([ T]))?(\d{2}:\d{2}(?::\d{2})?)(?:\.(\d+))?(Z|[+-]\d{2}(?::?\d{2})?(?::?\d{2})?)?$/.exec(text);
  if (!match || (shape.kind === "time" ? !!match[1] : !match[1]) || (shape.zone === "none" && match[5])) return null;
  const parts = { date: match[1] ?? "", separator: match[2] ?? "", time: match[3]!, fraction: match[4] ?? "", offset: match[5] ?? "" };
  return temporalError(parts, shape) ? null : parts;
}
export function freshTemporal(shape: TemporalShape, now = new Date()): TemporalParts {
  return {
    date: shape.kind === "time" ? "" : dayText(localDay(now)),
    separator: shape.kind === "timestamp" ? " " : "",
    time: shape.kind === "date" ? "" : `${pad(now.getHours())}:${pad(now.getMinutes())}:${pad(now.getSeconds())}`,
    fraction: "",
    offset:
      shape.kind !== "date" && shape.zone === "offset"
        ? `${now.getTimezoneOffset() > 0 ? "-" : "+"}${pad(Math.floor(Math.abs(now.getTimezoneOffset()) / 60))}:${pad(Math.abs(now.getTimezoneOffset()) % 60)}`
        : "",
  };
}
export const draftText = (draft: EditorDraft) => (draft.kind === "raw" ? draft.text : temporalText(draft.parts));
export function editOutcome(initial: InitialValue, draft: EditorDraft): EditOutcome {
  const text = draftText(draft);
  return !draft.edited || (initial.kind === "text" && initial.text === text) ? { kind: "unchanged" } : { kind: "value", value: text };
}
export function jsonError(text: string): string | null {
  const errors: ParseError[] = [];
  parseTree(text, errors, { allowTrailingComma: false, disallowComments: true });
  return errors.length ? `${printParseErrorCode(errors[0]!.error)} at character ${errors[0]!.offset + 1}.` : null;
}
export function formatJson(text: string): string {
  return applyEdits(text, format(text, undefined, { tabSize: 2, insertSpaces: true, eol: "\n" }));
}

// Textareas expose LF offsets even when the database source contains CRLF or CR.
function sourceOffset(source: string, visibleOffset: number): number {
  let at = 0;
  for (let visible = 0; visible < visibleOffset && at < source.length; visible++, at++) {
    if (source[at] === "\r" && source[at + 1] === "\n") at++;
  }
  return at;
}
export function spliceSource(source: string, start: number, end: number, inserted: string): string {
  return source.slice(0, sourceOffset(source, start)) + inserted + source.slice(sourceOffset(source, end));
}
export function textareaChange(source: string, next: string): string {
  const previous = source.replace(/\r\n?/g, "\n");
  let start = 0,
    suffix = 0;
  while (start < previous.length && start < next.length && previous[start] === next[start]) start++;
  while (suffix < previous.length - start && suffix < next.length - start && previous[previous.length - suffix - 1] === next[next.length - suffix - 1])
    suffix++;
  return spliceSource(source, start, previous.length - suffix, next.slice(start, next.length - suffix));
}
