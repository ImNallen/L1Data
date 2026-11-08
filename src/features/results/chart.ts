import type { ColumnMeta } from "../../ipc/bindings";

export type ChartKind = "Bar" | "Line";

/** Column indexes into the result. `x: null` plots against the row number. */
export type ChartSpec = { kind: ChartKind; x: number | null; y: number };

function isNumber(column: ColumnMeta): boolean {
  return column.type.category === "Integer" || column.type.category === "Numeric";
}

/** The columns a chart can plot as values. Money is text with a currency sign, so it never parses as a number. */
export function yColumns(columns: readonly ColumnMeta[]): number[] {
  return columns.flatMap((c, i) => (isNumber(c) && c.type.display !== "money" && c.type.display !== "smallmoney" ? [i] : []));
}

/** A line over time when the result has a temporal column, else bars by the first label column. */
export function defaultChart(columns: readonly ColumnMeta[]): ChartSpec | null {
  const y = yColumns(columns)[0];
  if (y === undefined) return null;
  const time = columns.findIndex((c, i) => i !== y && c.type.category === "Temporal");
  if (time >= 0) return { kind: "Line", x: time, y };
  const label = columns.findIndex((c, i) => i !== y && !isNumber(c));
  return { kind: "Bar", x: label >= 0 ? label : null, y };
}

/** Three to five round ticks, from the first at or below `min` to the first at or above `max`. */
export function niceTicks(min: number, max: number): number[] {
  if (min === max) {
    min = min > 0 ? 0 : min - 1;
    max = max < 0 ? 0 : max + 1;
  }
  const magnitude = 10 ** Math.floor(Math.log10((max - min) / 4));
  for (const m of [1, 2, 2.5, 5, 10, 20, 25, 50]) {
    const step = m * magnitude;
    const lo = Math.floor(min / step);
    const hi = Math.ceil(max / step);
    // Rounding at the step's own resolution strips float noise without merging ticks on a large offset.
    const decimals = Math.min(100, Math.max(0, 2 - Math.floor(Math.log10(step))));
    if (hi - lo <= 4) return Array.from({ length: hi - lo + 1 }, (_, i) => Number(((lo + i) * step).toFixed(decimals)));
  }
  return [min, max];
}
