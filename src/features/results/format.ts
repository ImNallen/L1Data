import type { TypeCategory } from "../../ipc/bindings";

const PLAIN_NUMBER = /^([+-]?)(\d+)(\.\d*)?([eE][+-]?\d+)?$/;

/**
 * Inserts thousands separators into PostgreSQL's text output of a number.
 * Works on the string so numeric(38,10) and int8 values keep every digit.
 * Anything that is not a plain decimal (NaN, Infinity, money's "$1,234.00") passes through.
 */
export function groupDigits(text: string): string {
  const m = PLAIN_NUMBER.exec(text);
  if (!m) return text;
  const [, sign = "", int = "", frac = "", exp = ""] = m;
  return sign + int.replace(/\B(?=(\d{3})+(?!\d))/g, ",") + frac + exp;
}

/** Numbers right-align in the grid and get thousands separators when measured. */
export function isNumeric(category: TypeCategory): boolean {
  return category === "Integer" || category === "Numeric";
}

const digits = (v: number) => groupDigits(String(v));

function totalLabel(last: number, shortPage: boolean, estimate: number | null, exactRowCount: number | undefined): string {
  if (exactRowCount !== undefined) return digits(exactRowCount);
  if (shortPage) return digits(last);
  if (estimate !== null && estimate > last) return `~${digits(estimate)}`;
  return `${digits(last)}+`;
}

export function pageStatus(
  page: number,
  size: number,
  rows: number,
  estimate: number | null,
  exactRowCount: number | undefined,
): { label: string; isLast: boolean } {
  const first = page * size;
  const last = first + rows;
  const isLast = exactRowCount !== undefined ? last >= exactRowCount : rows < size;
  if (rows === 0) return { label: `0 of ${digits(exactRowCount ?? first)}`, isLast };
  return { label: `${digits(first + 1)}–${digits(last)} of ${totalLabel(last, rows < size, estimate, exactRowCount)}`, isLast };
}

/** How many rows a table page holds, given the table's total row count. */
export const pageRows = (page: number, size: number, total: number | null): number | undefined =>
  total === null ? undefined : Math.min(size, Math.max(0, total - page * size));

export type RowWindow = { start: number; length: number };

export const PAGE_SIZES = [10, 100, 250, 500, 1000];

export const hasResultPager = (rowCount: number) => rowCount > PAGE_SIZES[0]!;

export function resultPage(
  rowCount: number,
  size: number,
  page: number,
): { page: number; lastPage: number; window: RowWindow; label: string; isLast: boolean } {
  const lastPage = Math.max(0, Math.ceil(rowCount / size) - 1);
  const shown = Math.min(page, lastPage);
  const start = shown * size;
  const length = Math.min(size, rowCount - start);
  return { page: shown, lastPage, window: { start, length }, ...pageStatus(shown, size, length, null, rowCount) };
}

export function formatElapsed(ms: number): string {
  if (ms < 1000) return `${ms} ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(ms < 10_000 ? 2 : 1)} s`;
  const m = Math.floor(ms / 60_000);
  const s = Math.round((ms % 60_000) / 1000);
  return `${m}m ${s}s`;
}

function tsvField(value: string | null): string {
  if (value === null) return "";
  return /[\t\n\r"]/.test(value) ? `"${value.replace(/"/g, '""')}"` : value;
}

export function toTsv(rows: readonly (readonly (string | null)[])[]): string {
  return rows.map((r) => r.map(tsvField).join("\t")).join("\n");
}

const TYPE_ALIASES: readonly [RegExp, string][] = [
  [/^timestamp(\(\d+\))? with time zone/, "timestamptz$1"],
  [/^timestamp(\(\d+\))? without time zone/, "timestamp$1"],
  [/^time(\(\d+\))? with time zone/, "timetz$1"],
  [/^time(\(\d+\))? without time zone/, "time$1"],
  [/^character varying/, "varchar"],
  [/^character\b/, "char"],
  [/^bit varying/, "varbit"],
];

export function shortType(display: string): string {
  for (const [long, short] of TYPE_ALIASES) if (long.test(display)) return display.replace(long, short);
  return display;
}

export function pluralRows(n: number): string {
  return `${groupDigits(String(n))} ${n === 1 ? "row" : "rows"}`;
}

const pad2 = (n: number) => String(n).padStart(2, "0");

export function formatClock(atMs: number): string {
  const d = new Date(atMs);
  return `${pad2(d.getHours())}:${pad2(d.getMinutes())}:${pad2(d.getSeconds())}`;
}
