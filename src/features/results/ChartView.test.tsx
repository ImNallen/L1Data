// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { commands, type ChartSeries, type ColumnMeta } from "../../ipc/bindings";
import type { RowResult } from "../../state/status";
import type { ChartSpec } from "./chart";
import { ChartView } from "./ChartView";

const columns: ColumnMeta[] = [
  { name: "region", type: { display: "text", category: "Text" }, source: null },
  { name: "revenue", type: { display: "numeric", category: "Numeric" }, source: null },
  { name: "orders", type: { display: "bigint", category: "Integer" }, source: null },
];

function result(rowCount: number, state: RowResult["state"] = "Complete"): RowResult {
  return {
    index: 0,
    plan: false,
    state,
    meta: {
      databaseKind: "Postgres",
      ref: { tabId: "t1", runId: 1, index: 0 },
      db: { connectionId: "c1", database: "shop" },
      columns,
      rowCount,
      rowsSeen: rowCount,
      truncated: null,
      elapsedMs: 5,
      allRows: { kind: "Statement" },
    },
  };
}

const ok = (series: ChartSeries) => ({ status: "ok" as const, data: series });
const bars: ChartSpec = { kind: "Bar", x: 0, y: 1 };

beforeEach(() => {
  vi.stubGlobal("PointerEvent", class extends MouseEvent {});
  vi.stubGlobal(
    "ResizeObserver",
    class {
      constructor(private readonly callback: ResizeObserverCallback) {}
      observe() {
        this.callback([{ contentRect: { width: 600, height: 300 } } as ResizeObserverEntry], this as unknown as ResizeObserver);
      }
      disconnect() {}
    },
  );
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

it("draws one bar per numeric value and names the chart", async () => {
  const series = vi.spyOn(commands, "resultSeries").mockResolvedValue(ok({ labels: ["north", "south", "east"], values: [120, null, 40.5], capped: false }));
  render(<ChartView result={result(3)} spec={bars} onSpec={() => {}} />);
  const plot = await screen.findByRole("img", { name: "Bar chart of revenue by region, 3 points" });
  expect(series).toHaveBeenCalledWith({ tabId: "t1", runId: 1, index: 0 }, 0, 1, 2000);
  expect(plot.querySelectorAll("path[fill='var(--color-run)']")).toHaveLength(2);
  expect(screen.getByTestId("chart-note").textContent).toBe("All 3 stored rows");
  expect(screen.getByText("north")).toBeTruthy();
});

it("says how many stored rows a capped series reads", async () => {
  vi.spyOn(commands, "resultSeries").mockResolvedValue(ok({ labels: ["1", "2"], values: [1, 2], capped: true }));
  render(<ChartView result={result(6915)} spec={{ kind: "Line", x: null, y: 1 }} onSpec={() => {}} />);
  await screen.findByRole("img", { name: "Line chart of revenue by row number, 2 points" });
  expect(screen.getByTestId("chart-note").textContent).toBe("First 2 of 6,915 stored rows");
});

it("shows the hovered bar's label and exact value", async () => {
  vi.spyOn(commands, "resultSeries").mockResolvedValue(ok({ labels: ["north", "south"], values: [1234.5678912, 10], capped: false }));
  render(<ChartView result={result(2)} spec={bars} onSpec={() => {}} />);
  const plot = await screen.findByRole("img");
  fireEvent.pointerMove(plot, { clientX: 100 });
  const tip = screen.getByTestId("chart-tooltip");
  expect(tip.textContent).toContain("1,234.5678912");
  expect(tip.textContent).toContain("revenue");
  expect(tip.textContent).toContain("north");
  fireEvent.pointerMove(plot, { clientX: 500 });
  expect(screen.getByTestId("chart-tooltip").textContent).toContain("south");
  fireEvent.pointerLeave(plot);
  expect(screen.queryByTestId("chart-tooltip")).toBeNull();
});

it("keeps the digits of a value too small for fixed decimals", async () => {
  vi.spyOn(commands, "resultSeries").mockResolvedValue(ok({ labels: ["a"], values: [0.0000001], capped: false }));
  render(<ChartView result={result(1)} spec={bars} onSpec={() => {}} />);
  fireEvent.pointerMove(await screen.findByRole("img"), { clientX: 300 });
  expect(screen.getByTestId("chart-tooltip").textContent).toContain("0.0000001");
});

it("says so when there are no rows", async () => {
  vi.spyOn(commands, "resultSeries").mockResolvedValue(ok({ labels: [], values: [], capped: false }));
  render(<ChartView result={result(0)} spec={bars} onSpec={() => {}} />);
  expect(await screen.findByText("No rows to chart.")).toBeTruthy();
});

it("offers the row number and every column as x, and only number columns as y", async () => {
  vi.spyOn(commands, "resultSeries").mockResolvedValue(ok({ labels: ["a"], values: [1], capped: false }));
  const onSpec = vi.fn();
  render(<ChartView result={result(1)} spec={bars} onSpec={onSpec} />);
  const user = userEvent.setup();
  await user.click(screen.getByTestId("chart-x"));
  expect(screen.getAllByRole("menuitem").map((m) => m.textContent)).toEqual(["Row number", "region", "revenue", "orders"]);
  await user.click(screen.getByTestId("chart-x-row"));
  expect(onSpec).toHaveBeenLastCalledWith({ kind: "Bar", x: null, y: 1 });
  await user.click(screen.getByTestId("chart-y"));
  expect(screen.getAllByRole("menuitem").map((m) => m.textContent)).toEqual(["revenue", "orders"]);
  await user.click(screen.getByTestId("chart-y-2"));
  expect(onSpec).toHaveBeenLastCalledWith({ kind: "Bar", x: 0, y: 2 });
});

it("keeps only the latest answer when the columns change mid-request", async () => {
  let answer: (series: ReturnType<typeof ok>) => void = () => {};
  vi.spyOn(commands, "resultSeries")
    .mockImplementationOnce(() => new Promise((resolve) => (answer = resolve)))
    .mockResolvedValueOnce(ok({ labels: ["by orders"], values: [7], capped: false }));
  const view = render(<ChartView result={result(1)} spec={bars} onSpec={() => {}} />);
  await waitFor(() => expect(commands.resultSeries).toHaveBeenCalledTimes(1));
  view.rerender(<ChartView result={result(1)} spec={{ ...bars, y: 2 }} onSpec={() => {}} />);
  await screen.findByText("by orders");
  await act(async () => answer(ok({ labels: ["stale"], values: [1], capped: false })));
  expect(screen.queryByText("stale")).toBeNull();
});

it("refetches at most every 300 ms while rows stream", async () => {
  vi.useFakeTimers();
  const series = vi.spyOn(commands, "resultSeries").mockResolvedValue(ok({ labels: ["a"], values: [1], capped: false }));
  const view = render(<ChartView result={result(1, "Streaming")} spec={bars} onSpec={() => {}} />);
  await act(async () => vi.advanceTimersByTimeAsync(0));
  expect(series).toHaveBeenCalledTimes(1);
  for (const rows of [2, 3, 4]) {
    view.rerender(<ChartView result={result(rows, "Streaming")} spec={bars} onSpec={() => {}} />);
    await act(async () => vi.advanceTimersByTimeAsync(50));
  }
  expect(series).toHaveBeenCalledTimes(1);
  await act(async () => vi.advanceTimersByTimeAsync(200));
  expect(series).toHaveBeenCalledTimes(2);
  view.rerender(<ChartView result={result(5)} spec={bars} onSpec={() => {}} />);
  await act(async () => vi.advanceTimersByTimeAsync(0));
  expect(series).toHaveBeenCalledTimes(3);
});
