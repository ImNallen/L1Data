import { mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, expect, it, vi } from "vitest";
import type { ConsoleState, ResultMeta } from "../../ipc/bindings";
import { useLive } from "../../state/live";
import { dropRowCaches, rowCacheFor } from "../../state/rowCache";
import { useWorkspace, type TableTab } from "../../state/workspace";
import { dropEditor, seedEditor, textOf } from "../editor/editorStates";
import { cycleTableOrder, filterForValue, referencedRows } from "./tableQuery";

const tab: TableTab = {
  id: "table",
  title: "odd table",
  console: { db: { connectionId: "ms", database: "shop" }, mode: "AutoCommit" },
  table: { schema: "dbo", table: "odd table", page: 0, query: { orderBy: "[odd name]", where: "", filters: [] } },
};
afterEach(() => {
  dropEditor(tab.id);
  dropRowCaches(tab.id);
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
  vi.unstubAllGlobals();
});

it("filters by a cell's value, using IS NULL for nulls", () => {
  expect(filterForValue("region", "EU-West", false)).toEqual({ column: "region", op: "Eq", value: "EU-West" });
  expect(filterForValue("region", "EU-West", true)).toEqual({ column: "region", op: "Ne", value: "EU-West" });
  expect(filterForValue("region", null, false)).toEqual({ column: "region", op: "IsNull", value: "" });
  expect(filterForValue("region", null, true)).toEqual({ column: "region", op: "NotNull", value: "" });
});

it("shows the referenced row with one equality filter per key column", () => {
  expect(referencedRows(["customer_id", "id"], ["7", "12"])).toEqual({
    orderBy: "",
    where: "",
    filters: [
      { column: "customer_id", op: "Eq", value: "7" },
      { column: "id", op: "Eq", value: "12" },
    ],
  });
});

it("cycles header ordering through the engine and stores its SQL unchanged", async () => {
  vi.stubGlobal("window", { crypto });
  useWorkspace.getState().addTab(tab);
  const requests: unknown[] = [];
  mockIPC((cmd, args) => {
    if (cmd === "table_order") {
      requests.push(args);
      return { sql: "[odd name] DESC", keys: [{ column: "odd name", desc: true }] };
    }
    return null;
  });
  await cycleTableOrder(tab, "odd name", true);
  expect(requests).toEqual([
    { db: tab.console.db, schema: "dbo", table: "odd table", orderBy: "[odd name]", edit: { kind: "Cycle", column: "odd name", additive: true } },
  ]);
  expect(useWorkspace.getState().tabs.table?.table?.query.orderBy).toBe("[odd name] DESC");
});

it("drops a sort reply after the user changes an advanced order expression", async () => {
  vi.stubGlobal("window", { crypto });
  useWorkspace.getState().addTab(tab);
  mockIPC((cmd) => {
    if (cmd === "table_order") {
      useWorkspace.getState().setTableQuery(tab.id, { ...tab.table.query, orderBy: "lower([odd name])" });
      return { sql: "[odd name] DESC", keys: [{ column: "odd name", desc: true }] };
    }
    return null;
  });
  await cycleTableOrder(tab, "odd name", false);
  expect(useWorkspace.getState().tabs.table?.table?.query.orderBy).toBe("lower([odd name])");
});

it("keeps rapid additive sorts in order using the previous engine reply", async () => {
  vi.stubGlobal("window", { crypto });
  useWorkspace.getState().addTab(tab);
  const orders: unknown[] = [];
  mockIPC((cmd, args) => {
    if (cmd === "table_order") {
      orders.push(args);
      return { sql: orders.length === 1 ? "[odd name] DESC" : "[odd name] DESC, [other]", keys: null };
    }
    return null;
  });
  await Promise.all([cycleTableOrder(tab, "odd name", false), cycleTableOrder(tab, "other", true)]);
  expect(orders).toEqual([
    { db: tab.console.db, schema: "dbo", table: "odd table", orderBy: "[odd name]", edit: { kind: "Cycle", column: "odd name", additive: false } },
    { db: tab.console.db, schema: "dbo", table: "odd table", orderBy: "[odd name] DESC", edit: { kind: "Cycle", column: "other", additive: true } },
  ]);
  expect(useWorkspace.getState().tabs.table?.table?.query.orderBy).toBe("[odd name] DESC, [other]");
});

it("keeps the query, editor and displayed rows when the engine refuses an additive header sort", async () => {
  vi.stubGlobal("window", { crypto });
  const raw: TableTab = { ...tab, table: { ...tab.table, page: 2, query: { ...tab.table.query, orderBy: "ID DESC" } } };
  useWorkspace.getState().addTab(raw);
  seedEditor(raw.id, "SELECT * FROM [dbo].[odd table]");
  const result = {
    databaseKind: "SqlServer",
    ref: { tabId: raw.id, runId: 7, index: 0 },
    db: raw.console.db,
    columns: [{ name: "id", type: { display: "int", category: "Integer" }, source: null }],
    rowCount: 2,
    rowsSeen: 2,
    truncated: null,
    elapsedMs: 1,
    allRows: { kind: "Table" },
  } satisfies ResultMeta;
  const console: ConsoleState = {
    tabId: raw.id,
    rev: 3,
    config: { connectionId: "ms", database: "shop", schema: "dbo", mode: "AutoCommit" },
    session: { kind: "Open", backendPid: 1 },
    txn: "Idle",
    txnDepth: 0,
    work: null,
    export: null,
    import: null,
    run: {
      kind: "Finished",
      runId: 7,
      elapsedMs: 1,
      outcome: "Ok",
      statements: [{ index: 0, range: { from: 0, to: 30 }, status: { kind: "Finished" }, outputs: [{ kind: "Rows", result, state: "Complete" }] }],
    },
  };
  useLive.getState().apply({ kind: "ConsoleChanged", ...console });
  useWorkspace.setState({ activeResult: { [raw.id]: 0 } });
  const message = "To add a header sort, ORDER BY must use exact table column names. Edit ORDER BY, or click without Shift to replace a simple column order.";
  const calls: string[] = [];
  mockIPC((cmd) => {
    calls.push(cmd);
    if (cmd === "result_page") return { start: 0, rows: [["2"], ["1"]], cut: [] };
    if (cmd === "table_order") return Promise.reject(message);
    return null;
  });
  const cache = rowCacheFor(result.ref);
  await cache.ensure({ start: 0, end: 2 }, 2);
  calls.length = 0;
  const shown = useLive.getState().consoles[raw.id];
  await cycleTableOrder(raw, "id", true);
  expect(calls).toEqual(["table_order"]);
  expect(useWorkspace.getState().tabs[raw.id]).toBe(raw);
  expect(textOf(raw.id)).toBe("SELECT * FROM [dbo].[odd table]");
  expect(useLive.getState().consoles[raw.id]).toBe(shown);
  expect(useWorkspace.getState().activeResult[raw.id]).toBe(0);
  expect([cache.get(0), cache.get(1)]).toEqual([["2"], ["1"]]);
  expect(useWorkspace.getState().toast?.text).toBe(message);
});

it("drops a sort reply after the tab changes table with the same database and query", async () => {
  vi.stubGlobal("window", { crypto });
  useWorkspace.getState().addTab(tab);
  mockIPC((cmd) => {
    if (cmd === "table_order") {
      useWorkspace.setState({ tabs: { [tab.id]: { ...tab, table: { ...tab.table, schema: "other", table: "other table" } } } });
      return { sql: "[odd name] DESC", keys: [{ column: "odd name", desc: true }] };
    }
    return null;
  });
  await cycleTableOrder(tab, "odd name", false);
  expect(useWorkspace.getState().tabs.table?.table).toMatchObject({ schema: "other", table: "other table", query: tab.table.query });
});
