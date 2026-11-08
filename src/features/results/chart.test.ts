import { describe, expect, it } from "vitest";
import type { ColumnMeta, TypeCategory } from "../../ipc/bindings";
import { defaultChart, niceTicks, yColumns } from "./chart";

const col = (name: string, category: TypeCategory, display = category.toLowerCase()): ColumnMeta => ({ name, type: { display, category }, source: null });

describe("yColumns", () => {
  it("keeps integer and numeric columns but not money", () => {
    const columns = [
      col("id", "Integer", "integer"),
      col("name", "Text"),
      col("total", "Numeric", "numeric"),
      col("price", "Numeric", "money"),
      col("fee", "Numeric", "smallmoney"),
      col("ratio", "Numeric", "double precision"),
    ];
    expect(yColumns(columns)).toEqual([0, 2, 5]);
  });

  it("is empty for a result with no number column", () => {
    expect(yColumns([col("t", "Text"), col("day", "Temporal")])).toEqual([]);
  });
});

describe("defaultChart", () => {
  it("has no chart without a number column", () => {
    expect(defaultChart([col("t", "Text")])).toBeNull();
    expect(defaultChart([col("price", "Numeric", "money")])).toBeNull();
  });

  it("draws bars by the first label column", () => {
    expect(defaultChart([col("region", "Text"), col("revenue", "Numeric")])).toEqual({ kind: "Bar", x: 0, y: 1 });
    expect(defaultChart([col("revenue", "Numeric"), col("id", "Integer"), col("ok", "Bool")])).toEqual({ kind: "Bar", x: 2, y: 0 });
  });

  it("draws a line over the first temporal column, ahead of a label column", () => {
    expect(defaultChart([col("region", "Text"), col("revenue", "Numeric"), col("day", "Temporal"), col("at", "Temporal")])).toEqual({
      kind: "Line",
      x: 2,
      y: 1,
    });
  });

  it("plots against the row number when every other column is a number", () => {
    expect(defaultChart([col("n", "Integer"), col("r", "Numeric")])).toEqual({ kind: "Bar", x: null, y: 0 });
    expect(defaultChart([col("n", "Integer")])).toEqual({ kind: "Bar", x: null, y: 0 });
  });
});

describe("niceTicks", () => {
  it("covers the range with three to five round ticks", () => {
    expect(niceTicks(0, 87)).toEqual([0, 25, 50, 75, 100]);
    expect(niceTicks(0, 1)).toEqual([0, 0.25, 0.5, 0.75, 1]);
    expect(niceTicks(-3.2, 7.9)).toEqual([-5, 0, 5, 10]);
    expect(niceTicks(0, 1_234_567)).toEqual([0, 500_000, 1_000_000, 1_500_000]);
    expect(niceTicks(0.1, 0.3)).toEqual([0.1, 0.15, 0.2, 0.25, 0.3]);
  });

  it("widens a single value to a range", () => {
    expect(niceTicks(0, 0)).toEqual([-1, -0.5, 0, 0.5, 1]);
    expect(niceTicks(5, 5)).toEqual([0, 2, 4, 6]);
  });

  it("keeps every count between three and five", () => {
    for (const [min, max] of [
      [0, 9],
      [0, 11],
      [3, 97],
      [-0.004, 0.019],
      [12, 13],
      [1e9, 3.3e9],
    ] as const) {
      const ticks = niceTicks(min, max);
      expect(ticks.length).toBeGreaterThanOrEqual(3);
      expect(ticks.length).toBeLessThanOrEqual(5);
      expect(ticks[0]).toBeLessThanOrEqual(min);
      expect(ticks.at(-1)).toBeGreaterThanOrEqual(max);
    }
  });

  it("keeps ticks distinct on a large offset with a small range", () => {
    const ticks = niceTicks(1_000_000_000_000, 1_000_000_000_002);
    expect(new Set(ticks).size).toBe(ticks.length);
    expect(ticks[0]).toBeLessThanOrEqual(1_000_000_000_000);
    expect(ticks.at(-1)).toBeGreaterThanOrEqual(1_000_000_000_002);
  });
});
