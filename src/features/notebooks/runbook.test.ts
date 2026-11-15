import { describe, expect, it } from "vitest";
import type { RunState, StatementRun } from "../../ipc/bindings";
import { afterCell, chooseCells, horizon, progressText, ranNotice, stoppedNotice, type CellFacts, type NotebookRun } from "./runbook";

const cells: CellFacts[] = [
  { id: "a", runnable: true, stale: false },
  { id: "blank", runnable: false, stale: false },
  { id: "b", runnable: true, stale: true },
  { id: "c", runnable: true, stale: false },
  { id: "d", runnable: true, stale: true },
];

describe("chooseCells", () => {
  it("runs every runnable cell in notebook order", () => {
    expect(chooseCells(cells, "all", null)).toEqual(["a", "b", "c", "d"]);
  });

  it("runs only the stale cells", () => {
    expect(chooseCells(cells, "stale", "c")).toEqual(["b", "d"]);
  });

  it("runs from the active cell to the end, and nothing without one", () => {
    expect(chooseCells(cells, "fromActive", "c")).toEqual(["c", "d"]);
    expect(chooseCells(cells, "fromActive", "blank")).toEqual(["b", "c", "d"]);
    expect(chooseCells(cells, "fromActive", null)).toEqual([]);
  });
});

describe("horizon", () => {
  it("covers every cell when each can be prepared", () => {
    expect(horizon([[], [{ name: ":x" }], []])).toBe(2);
  });

  it("stops at the first cell that cannot be prepared", () => {
    expect(horizon([[], null, [], null])).toBe(1);
  });
});

function finished(outcome: "Ok" | "Failed" | "Cancelled", statements: StatementRun[] = []): { kind: "finished"; run: Extract<RunState, { kind: "Finished" }> } {
  return { kind: "finished", run: { kind: "Finished", runId: 1, elapsedMs: 5, statements, outcome } };
}

const failedStatement: StatementRun = {
  index: 0,
  range: { from: 0, to: 8 },
  outputs: [],
  status: {
    kind: "Failed",
    error: { severity: "ERROR", sqlstate: "22012", message: "division by zero", detail: null, hint: null, context: null, position: null },
  },
};

describe("afterCell", () => {
  it("goes on after a cell that finishes Ok, even when Stop came meanwhile", () => {
    expect(afterCell("orders", finished("Ok"), false)).toEqual({ kind: "next" });
    expect(afterCell("orders", finished("Ok"), true)).toEqual({ kind: "next" });
  });

  it("stops at a failed cell with its first error, and shows the cell", () => {
    expect(afterCell("orders", finished("Failed", [failedStatement]), false)).toEqual({
      kind: "failed",
      notice: "Stopped at orders: division by zero",
      reveal: true,
    });
  });

  it("names the cell the engine refused", () => {
    expect(afterCell("orders", { kind: "refused", error: { kind: "Busy" } }, false)).toEqual({
      kind: "failed",
      notice: "Stopped at orders: This tab is still running. Cancel it first.",
      reveal: true,
    });
  });

  it("skips a cell with only comments", () => {
    expect(afterCell("notes", { kind: "refused", error: { kind: "NoStatement" } }, false)).toEqual({ kind: "skipped" });
  });

  it("tells a cancel made in the cell from Stop", () => {
    expect(afterCell("orders", finished("Cancelled"), false)).toEqual({ kind: "failed", notice: "orders was cancelled.", reveal: true });
    expect(afterCell("orders", finished("Cancelled"), true)).toEqual({ kind: "stopped" });
  });

  it("stops without showing a closed cell, and quietly after a dropped send", () => {
    expect(afterCell("orders", { kind: "gone" }, false)).toEqual({ kind: "failed", notice: "orders was closed.", reveal: false });
    expect(afterCell("orders", { kind: "dropped" }, false)).toEqual({ kind: "dropped" });
  });
});

describe("notices and progress", () => {
  it("counts the cells that ran, or the ones Stop left", () => {
    expect(ranNotice(1)).toBe("Ran 1 cell.");
    expect(stoppedNotice(3)).toBe("Stopped. 3 cells did not run.");
  });

  it("shows which cell of how many runs", () => {
    const run: NotebookRun = { notebook: "n", cells: ["a", "b", "c", "d"], at: 1, phase: "running" };
    expect(progressText(run)).toBe("2 of 4");
    expect(progressText({ ...run, phase: "stopping" })).toBe("");
    expect(progressText({ ...run, phase: "preparing" })).toBe("");
  });
});
