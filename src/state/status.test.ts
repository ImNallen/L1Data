import { describe, expect, it } from "vitest";
import type { ConnectionConfig, ConsoleState, OutputEntry, QueryError, ResultMeta, RunOutput, RunState, StatementRun, Workspace } from "../ipc/bindings";
import { hasUncommittedWork, outputTone, resultView, rowResults, runSummary, shownResult } from "./status";
import { nextUntitled, tabFromSnapshot, workspaceSnapshot, type Tab } from "./workspace";

const meta = (index: number, rowCount: number, extra: Partial<ResultMeta> = {}): ResultMeta => ({
  ref: { tabId: "t", runId: 1, index },
  db: { connectionId: "c", database: "shop" },
  databaseKind: "Postgres",
  columns: [],
  rowCount,
  rowsSeen: rowCount,
  truncated: null,
  elapsedMs: 38,
  allRows: { kind: "Statement" },
  ...extra,
});
const stmt = (index: number, status: StatementRun["status"], outputs: RunOutput[] = []): StatementRun => ({
  index,
  range: { from: 0, to: 1 },
  status,
  outputs,
});
const rows = (index: number, result: ResultMeta) => stmt(index, { kind: "Finished" }, [{ kind: "Rows", result, state: "Complete" }]);
const command = (index: number, tag: string, elapsedMs: number) => stmt(index, { kind: "Finished" }, [{ kind: "Command", tag, elapsedMs, affectedRows: null }]);
const finished = (statements: StatementRun[], outcome: "Ok" | "Failed" | "Cancelled" = "Ok"): RunState => ({
  kind: "Finished",
  runId: 1,
  elapsedMs: 50,
  statements,
  outcome,
});

function consoleWith(mode: "AutoCommit" | "Manual", txn: ConsoleState["txn"]): ConsoleState {
  return {
    tabId: "t",
    rev: 1,
    config: { connectionId: "c", database: "shop", schema: null, mode },
    session: { kind: "None" },
    txnDepth: null,
    work: null,
    export: null,
    import: null,
    txn,
    run: { kind: "Idle" },
  };
}

describe("hasUncommittedWork", () => {
  it("is true whenever a console holds a transaction", () => {
    expect(hasUncommittedWork(consoleWith("Manual", "Active"))).toBe(true);
    expect(hasUncommittedWork(consoleWith("Manual", "Aborted"))).toBe(true);
    expect(hasUncommittedWork(consoleWith("Manual", "Idle"))).toBe(false);
    expect(hasUncommittedWork(consoleWith("AutoCommit", "Active"))).toBe(true);
    expect(hasUncommittedWork(undefined)).toBe(false);
  });
});

describe("runSummary", () => {
  it("reads like the mockup for a finished row result", () => {
    expect(runSummary(finished([rows(0, meta(0, 6))]), undefined, 0)).toEqual({
      tone: "ok",
      text: "6 rows · 38 ms",
    });
  });

  it("names a truncated result's limit and the rows the server sent", () => {
    const run = finished([rows(0, meta(0, 200_000, { rowsSeen: 300_000, truncated: "RowLimit" }))]);
    expect(runSummary(run, undefined, 0)?.text).toBe("200,000 of 300,000 rows (row limit) · 38 ms");
  });

  it("shows the first error of a failed run", () => {
    const error = { severity: "ERROR", sqlstate: "42P01", message: 'relation "nope" does not exist', detail: null, hint: null, context: null, position: 14 };
    expect(runSummary(finished([stmt(0, { kind: "Failed", error })], "Failed"), undefined, 0)).toEqual({ tone: "error", text: error.message });
  });

  it("falls back to the last command tag when no statement returned rows", () => {
    const run = finished([command(0, "UPDATE 3", 4)]);
    expect(runSummary(run, undefined, 0)?.text).toBe("UPDATE 3 · 4 ms");
  });

  it("counts elapsed time from startedAtMs while running", () => {
    const run: RunState = { kind: "Running", runId: 1, startedAtMs: 1_000, statements: [stmt(0, { kind: "Preparing" })] };
    expect(runSummary(run, undefined, 3_500)).toEqual({ tone: "running", text: "Running · 2.50 s" });
  });
});

describe("shownResult", () => {
  const run = finished([rows(0, meta(0, 1)), command(1, "UPDATE 1", 1), rows(2, meta(2, 9))]);

  it("defaults to the last row result and honours a pick that still exists", () => {
    expect(rowResults(run).map((r) => r.index)).toEqual([0, 2]);
    expect(shownResult(run, undefined)?.index).toBe(2);
    expect(shownResult(run, 0)?.index).toBe(0);
    expect(shownResult(run, 1)?.index).toBe(2);
  });
});

const notice = (runId: number, message: string, severity = "NOTICE"): OutputEntry => ({
  atMs: 0,
  runId,
  body: { kind: "Notice", severity, message, detail: null },
});
const failure: QueryError = { severity: "ERROR", sqlstate: "40001", message: "could not serialize", detail: null, hint: null, context: null, position: null };
const finishedRun = (runId: number, statements: StatementRun[] = []): RunState => ({ kind: "Finished", runId, elapsedMs: 5, statements, outcome: "Ok" });
const rowsAt = (index: number) => rows(index, meta(index, 1));
const planAt = (index: number) => stmt(index, { kind: "Finished" }, [{ kind: "Plan", result: meta(index, 1), format: "Json" }]);

describe("resultView", () => {
  const withRows = finishedRun(7, [rowsAt(0), rowsAt(1)]);

  it("shows an Output pick made on the current run over the grid", () => {
    expect(resultView(withRows, { kind: "output", runId: 7, result: undefined }, true)).toEqual({ kind: "output" });
  });

  it("keeps the picked result set behind an Output pick", () => {
    expect(shownResult(withRows, { kind: "output", runId: 7, result: 0 })).toMatchObject({ index: 0 });
    expect(resultView(withRows, { kind: "output", runId: 7, result: 0 }, false)).toMatchObject({ kind: "rows", result: { index: 0 } });
  });

  it("forgets the result set behind a lapsed Output pick", () => {
    expect(shownResult(finishedRun(8, [rowsAt(0), rowsAt(1), rowsAt(2)]), { kind: "output", runId: 7, result: 0 })).toMatchObject({ index: 2 });
  });

  it("drops an Output pick on the next run", () => {
    expect(resultView(finishedRun(8, [rowsAt(0)]), { kind: "output", runId: 7, result: undefined }, true)).toMatchObject({
      kind: "rows",
      result: { index: 0 },
    });
  });

  it("prefers the grid to the log without a pick", () => {
    expect(resultView(withRows, undefined, true)).toMatchObject({ kind: "rows", result: { index: 1 } });
    expect(resultView(withRows, 0, true)).toMatchObject({ kind: "rows", result: { index: 0 } });
  });

  it("shows the log instead of the tail when the tab has entries", () => {
    expect(resultView(finishedRun(7, [command(0, "DO", 1)]), undefined, true)).toEqual({ kind: "output" });
  });

  it("shows a plan as a tree until the rows view is chosen", () => {
    const run = finishedRun(7, [planAt(0)]);
    expect(resultView(run, undefined, false)).toMatchObject({ kind: "plan", result: { index: 0, plan: true } });
    expect(resultView(run, undefined, false, true)).toMatchObject({ kind: "rows", result: { index: 0, plan: true } });
    expect(runSummary(finished([planAt(0)]), undefined, 0)).toEqual({ tone: "ok", text: "Plan · 38 ms" });
  });

  it("falls back to the tail when the log is empty", () => {
    expect(resultView(finishedRun(7, [command(0, "DO", 1)]), undefined, false)).toEqual({ kind: "tail" });
    expect(resultView(finishedRun(7), { kind: "output", runId: 7, result: undefined }, false)).toEqual({ kind: "tail" });
    expect(resultView({ kind: "Idle" }, undefined, false)).toEqual({ kind: "tail" });
  });
});

describe("outputTone", () => {
  const command = (runId: number): OutputEntry => ({ atMs: 0, runId, body: { kind: "Command", statement: 0, tag: "DO", elapsedMs: 1 } });
  const error = (runId: number): OutputEntry => ({ atMs: 0, runId, body: { kind: "Error", statement: 0, error: failure } });

  it("turns amber for a WARNING in the latest run", () => {
    expect(outputTone([notice(7, "careful", "WARNING"), command(7)])).toBe("warn");
  });

  it("stays muted once a later run raised no WARNING", () => {
    expect(outputTone([notice(7, "careful", "WARNING"), command(7), command(8)])).toBe("muted");
  });

  it("stays muted for other notices and for errors", () => {
    expect(outputTone([notice(7, "fyi"), notice(7, "info", "INFO"), error(7)])).toBe("muted");
    expect(outputTone([])).toBe("muted");
  });
});

describe("workspace snapshot", () => {
  const configs: Record<string, ConnectionConfig> = {
    c: {
      databaseKind: "Postgres",
      id: "c",
      name: "dev",
      host: "localhost",
      port: 5433,
      database: "shop",
      user: "l1data",
      sslMode: "Prefer",
      caCertPath: null,
      hasPassword: true,
      color: null,
      ssh: null,
      environment: null,
      sqlServerAuth: null,
    },
  };
  const tabs: Tab[] = [
    { id: "a", title: "Untitled-2.sql", console: { db: { connectionId: "c", database: "reporting" }, mode: "Manual" }, table: null, file: null },
    {
      id: "b",
      title: "orders",
      console: { db: { connectionId: "c", database: "shop" }, mode: "Manual" },
      table: {
        schema: "public",
        table: "orders",
        page: 3,
        query: { orderBy: "total DESC", filters: [{ column: "status", op: "Eq", value: "paid" }], where: "total > 10" },
      },
      file: null,
    },
  ];
  const state = {
    tabs: Object.fromEntries(tabs.map((t) => [t.id, t])),
    order: ["b", "a"],
    active: "a",
    layout: {
      sidebar: true,
      results: false,
      rightPanel: "Assistant" as const,
      resultsHeight: 280,
      sidebarWidth: 256,
      rightPanelWidth: 341,
      assistantChat: "c4a70000-0000-4000-8000-000000000001",
    },
  };

  it("round-trips every tab's text and mode, and a table tab's binding", () => {
    const snap = workspaceSnapshot(state, { textOf: (id) => `-- text of ${id}`, isDirty: () => false });
    expect(snap.tabs).toEqual([
      {
        id: "b",
        title: "orders",
        text: "-- text of b",
        console: { connectionId: "c", database: "shop", schema: null, mode: "Manual" },
        table: {
          schema: "public",
          table: "orders",
          page: 3,
          query: { orderBy: "total DESC", filters: [{ column: "status", op: "Eq", value: "paid" }], where: "total > 10" },
        },
        file: null,
      },
      {
        id: "a",
        title: "Untitled-2.sql",
        text: "-- text of a",
        console: { connectionId: "c", database: "reporting", schema: null, mode: "Manual" },
        table: null,
        file: null,
      },
    ]);
    expect(snap.tabs.map((t) => tabFromSnapshot(t, configs))).toEqual([tabs[1], tabs[0]]);
    expect(snap.layout).toEqual(state.layout);
  });

  it("restores a table tab saved before tabs were one shape as an auto-commit tab named after its table", () => {
    // What the engine reads from an old `{"kind":"Table", ...}` row.
    const saved = {
      id: "b",
      title: "orders",
      text: null,
      console: { connectionId: "c", database: "shop", schema: null, mode: "AutoCommit" as const },
      table: { schema: "public", table: "orders", page: 2, query: { orderBy: "", filters: [], where: "" } },
      file: null,
    };
    expect(tabFromSnapshot(saved, configs)).toEqual({
      id: "b",
      title: "orders",
      console: { db: { connectionId: "c", database: "shop" }, mode: "AutoCommit" },
      table: { schema: "public", table: "orders", page: 2, query: { orderBy: "", filters: [], where: "" } },
      file: null,
    });
  });

  it("restores a tab saved before tabs carried a database onto the connection's configured database", () => {
    const saved: Workspace["tabs"] = [
      { id: "a", title: "q.sql", text: "select 1", console: { connectionId: "c", database: null, schema: "lab", mode: "AutoCommit" }, table: null, file: null },
      { id: "d", title: "free.sql", text: "", console: { connectionId: null, database: null, schema: null, mode: "Manual" }, table: null, file: null },
    ];
    expect(saved.map((t) => tabFromSnapshot(t, configs))).toEqual([
      { id: "a", title: "q.sql", console: { db: { connectionId: "c", database: "shop" }, mode: "AutoCommit", schema: "lab" }, table: null, file: null },
      { id: "d", title: "free.sql", console: { db: null, mode: "Manual" }, table: null, file: null },
    ]);
  });

  it("keeps a tab whose connection is gone as a plain query tab with no target", () => {
    const saved = {
      id: "b",
      title: "orders",
      text: "select * from orders",
      console: { connectionId: "gone", database: "shop", schema: null, mode: "Manual" as const },
      table: { schema: "public", table: "orders", page: 0, query: { orderBy: "", filters: [], where: "" } },
      file: null,
    };
    expect(tabFromSnapshot(saved, configs)).toEqual({ id: "b", title: "orders", console: { db: null, mode: "Manual" }, table: null, file: null });
  });

  it("numbers new tabs after the highest Untitled", () => {
    expect(nextUntitled(tabs)).toBe("Untitled-3.sql");
    expect(nextUntitled([])).toBe("Untitled-1.sql");
  });
});

it("keeps every result ordinal from one failed batch, including empty and partial row sets", () => {
  const run = finished(
    [
      stmt(0, { kind: "Failed", error: failure }, [
        { kind: "Rows", result: meta(0, 0, { databaseKind: "SqlServer" }), state: "Complete" },
        { kind: "Command", tag: "UPDATE", affectedRows: null, elapsedMs: 1 },
        { kind: "Rows", result: meta(1, 2, { databaseKind: "SqlServer" }), state: "Failed" },
      ]),
    ],
    "Failed",
  );
  expect(rowResults(run).map((r) => [r.index, r.meta.rowCount, r.state])).toEqual([
    [0, 0, "Complete"],
    [1, 2, "Failed"],
  ]);
  expect(shownResult(run, 0)?.meta.rowCount).toBe(0);
  expect(resultView(run, 1, true)).toMatchObject({ kind: "rows", result: { index: 1 } });
  expect(runSummary(run, 1, 0)).toEqual({ tone: "error", text: failure.message });
});
