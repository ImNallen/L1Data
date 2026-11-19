// @vitest-environment jsdom
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ColumnMeta, ConsoleState, DatabaseKind, ResultMeta, RunOutput, TableScript } from "../../ipc/bindings";
import { useLive } from "../../state/live";
import { dropRowCaches } from "../../state/rowCache";
import { EMPTY_QUERY, useWorkspace, type Tab, type TableTab } from "../../state/workspace";
import { dropEditor, seedEditor } from "../editor/editorStates";
import { browseTemplate } from "../editor/runActions";
import { ResultsPane } from "./ResultsPane";
import { cellsState } from "../../testing/notebooks";
import { connectionConfig } from "../../testing/connections";
import { shopCatalog } from "../../testing/shopCatalog";
import { idleConsole, tableRun } from "../../testing/tableRun";
import { buildCatalogView, useCatalog } from "../../state/catalog";
import { dbKey } from "../../state/dbKey";

const db = { connectionId: "c1", database: "shop" };
const tab: TableTab = {
  id: "t1",
  title: "customers",
  console: { db, mode: "AutoCommit" },
  table: { schema: "public", table: "customers", page: 1, query: EMPTY_QUERY },
};
const script: TableScript = { text: "SELECT * FROM public.customers", acceptedTexts: ["SELECT * FROM public.customers"] };

beforeEach(async () => {
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      unobserve() {}
      disconnect() {}
    },
  );
  mockIPC((cmd) => {
    if (cmd === "table_script") return script;
    if (cmd === "table_order") return [];
    if (cmd === "result_page") return { start: 0, rows: Array.from({ length: 100 }, (_, i) => [String(i)]), cut: [] };
    throw new Error(`Unexpected IPC command: ${cmd}`);
  });
  useWorkspace.setState({
    ...cellsState([tab]),
    settings: { ...useWorkspace.getState().settings, tablePageSize: 1000, maxRows: 100 },
  });
  await browseTemplate(db, tab.table.schema, tab.table.table);
  seedEditor(tab.id, script.text);
});

afterEach(() => {
  cleanup();
  clearMocks();
  for (const id of [tab.id, "q1"]) {
    dropRowCaches(id);
    dropEditor(id);
  }
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
  useCatalog.setState(useCatalog.getInitialState(), true);
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

it.each([
  { truncated: "RowLimit", rowsSeen: 1000, range: "1,001–2,000 of 2,000+", isLast: false, limit: "row limit" },
  { truncated: "ByteLimit", rowsSeen: 1000, range: "1,001–2,000 of 2,000+", isLast: false, limit: "size limit" },
  { truncated: "RowLimit", rowsSeen: 400, range: "1,001–1,400 of 1,400", isLast: true, limit: "row limit" },
] satisfies { truncated: ResultMeta["truncated"]; rowsSeen: number; range: string; isLast: boolean; limit: string }[])(
  "pages by server rows with $truncated and $rowsSeen rows received",
  async ({ truncated, rowsSeen, range, isLast, limit }) => {
    const result: ResultMeta = {
      databaseKind: "Postgres",
      ref: { tabId: tab.id, runId: 1, index: 0 },
      db,
      columns: [{ name: "id", type: { display: "integer", category: "Integer" }, source: null }],
      rowCount: 100,
      rowsSeen,
      truncated,
      elapsedMs: 5,
      allRows: { kind: "Statement" },
      repeatedRead: null,
    };
    const state: ConsoleState = {
      tabId: tab.id,
      rev: 1,
      config: { ...db, schema: null, mode: "AutoCommit" },
      session: { kind: "None" },
      txn: "Idle",
      txnDepth: null,
      work: null,
      export: null,
      import: null,
      run: {
        kind: "Finished",
        runId: 1,
        elapsedMs: 5,
        outcome: "Ok",
        statements: [
          { index: 0, range: { from: 0, to: script.text.length }, status: { kind: "Finished" }, outputs: [{ kind: "Rows", result, state: "Complete" }] },
        ],
      },
    };
    useLive.setState({ consoles: { [tab.id]: state } });
    await act(async () => {
      render(<ResultsPane tabId={tab.id} />);
    });

    expect.soft(screen.getByRole("button", { name: "Next page" }).hasAttribute("disabled"), "Next page disabled").toBe(isLast);
    expect.soft(screen.getByRole("button", { name: "Last page" }).hasAttribute("disabled"), "Last page disabled").toBe(isLast);
    expect(screen.getByRole("button", { name: `${range}, rows per page` })).toBe(screen.getByTestId("table-page"));
    expect(screen.getByTestId("table-pager")).toBeTruthy();
    expect(screen.queryByTestId("result-pager")).toBeNull();
    expect(screen.getByTestId("result-status").textContent).toContain(`100 of ${rowsSeen.toLocaleString("en-US")} rows (${limit})`);
  },
);

function finishedWith(output: (meta: ResultMeta) => RunOutput, columns: ColumnMeta[]): ConsoleState {
  const meta: ResultMeta = {
    databaseKind: "Postgres",
    ref: { tabId: tab.id, runId: 2, index: 0 },
    db,
    columns,
    rowCount: 2,
    rowsSeen: 2,
    truncated: null,
    elapsedMs: 5,
    allRows: { kind: "Statement" },
    repeatedRead: null,
  };
  return {
    tabId: tab.id,
    rev: 1,
    config: { ...db, schema: null, mode: "AutoCommit" },
    session: { kind: "None" },
    txn: "Idle",
    txnDepth: null,
    work: null,
    export: null,
    import: null,
    run: {
      kind: "Finished",
      runId: 2,
      elapsedMs: 5,
      outcome: "Ok",
      statements: [{ index: 0, range: { from: 0, to: 1 }, status: { kind: "Finished" }, outputs: [output(meta)] }],
    },
  };
}

const region: ColumnMeta = { name: "region", type: { display: "text", category: "Text" }, source: null };
const revenue: ColumnMeta = { name: "revenue", type: { display: "numeric", category: "Numeric" }, source: null };

it("switches a result with a number column between rows and a chart", async () => {
  mockIPC((cmd) => {
    if (cmd === "table_script") return script;
    if (cmd === "table_order") return [];
    if (cmd === "result_page")
      return {
        start: 0,
        rows: [
          ["north", "10"],
          ["south", "4"],
        ],
        cut: [],
      };
    if (cmd === "result_series") return { labels: ["north", "south"], values: [10, 4], capped: false };
    throw new Error(`Unexpected IPC command: ${cmd}`);
  });
  useLive.setState({ consoles: { [tab.id]: finishedWith((result) => ({ kind: "Rows", result, state: "Complete" }), [region, revenue]) } });
  await act(async () => {
    render(<ResultsPane tabId={tab.id} />);
  });
  const rows = screen.getByTestId("view-rows");
  const chart = screen.getByTestId("view-chart");
  expect(rows.getAttribute("aria-pressed")).toBe("true");
  expect(chart.getAttribute("aria-pressed")).toBe("false");
  await act(async () => fireEvent.click(chart));
  expect(chart.getAttribute("aria-pressed")).toBe("true");
  expect(await screen.findByRole("img", { name: "Bar chart of revenue by region, 2 points" })).toBeTruthy();
  await act(async () => fireEvent.click(rows));
  expect(screen.queryByTestId("chart-view")).toBeNull();
});

it("offers no chart for a result without a number column, or for a plan", async () => {
  useLive.setState({ consoles: { [tab.id]: finishedWith((result) => ({ kind: "Rows", result, state: "Complete" }), [region]) } });
  const view = await act(async () => render(<ResultsPane tabId={tab.id} />));
  expect(screen.queryByTestId("view-chart")).toBeNull();
  mockIPC((cmd) => {
    if (cmd === "result_plan") return { statements: [] };
    throw new Error(`Unexpected IPC command: ${cmd}`);
  });
  useLive.setState({ consoles: { [tab.id]: finishedWith((result) => ({ kind: "Plan", result, format: "Json" }), [revenue]) } });
  await act(async () => view.rerender(<ResultsPane tabId={tab.id} />));
  expect(screen.queryByTestId("view-chart")).toBeNull();
  expect(screen.getByTestId("plan-rows-toggle")).toBeTruthy();
});

it("says a SQL Server view's rows are read-only, without asking the engine to check it for edits", async () => {
  const catalog = { ...shopCatalog("c1", 1), databaseKind: "SqlServer" as const };
  const customers = catalog.schemas[1]!.relations.find((r) => r.name === "customers")!;
  customers.kind = "View";
  useCatalog.setState({ views: { [dbKey(db)]: buildCatalogView(catalog) } });
  useWorkspace.setState({ configs: { c1: { ...connectionConfig("c1", "mssql"), databaseKind: "SqlServer" } } });
  await browseTemplate(db, tab.table.schema, tab.table.table);
  const viewRegion = { ...region, source: { tableOid: customers.oid, attnum: 4 } };
  const state = finishedWith((result) => ({ kind: "Rows", result: { ...result, databaseKind: "SqlServer" }, state: "Complete" }), [viewRegion]);
  useLive.setState({ consoles: { [tab.id]: state } });
  await act(async () => {
    render(<ResultsPane tabId={tab.id} />);
  });
  expect(screen.getByTestId("edit-refusal").textContent).toBe("Views are read-only here. Use a SQL script to change their rows.");
});

const queryTab: Tab = { id: "q1", title: "Query 1", console: { db, mode: "AutoCommit" }, table: null };

async function showQuery(databaseKind: DatabaseKind, serve: (cmd: string, args: unknown) => unknown = () => undefined) {
  const catalog = { ...shopCatalog("c1", 1), databaseKind };
  useCatalog.setState({ views: { [dbKey(db)]: buildCatalogView(catalog) } });
  useWorkspace.setState({ ...cellsState([queryTab]), configs: { c1: { ...connectionConfig("c1", "local"), databaseKind } } });
  seedEditor(queryTab.id, "SELECT * FROM public.orders");
  mockIPC((cmd, args) => {
    if (cmd === "result_page") return { start: 0, rows: [["1", "7", "new", "10", "2026-11-19"]], cut: [] };
    const reply = serve(cmd, args);
    if (reply !== undefined) return reply;
    throw new Error(`Unexpected IPC command: ${cmd}`);
  });
  useLive.setState({ consoles: { q1: { ...idleConsole("q1", db), run: tableRun("q1", catalog, "public", "orders", { kind: "Statement" }) } } });
  await act(async () => {
    render(<ResultsPane tabId="q1" />);
  });
}

it("edits a query tab's single-table result, as a table tab does", async () => {
  await showQuery("Postgres");
  expect(screen.getByTestId("add-row").hasAttribute("disabled")).toBe(false);
  expect(screen.queryByTestId("edit-refusal")).toBeNull();
});

it("checks a SQL Server query result's table for edits, and says why it refuses", async () => {
  const asked: unknown[] = [];
  await showQuery("SqlServer", (cmd, args) => {
    if (cmd !== "table_edit_support") return undefined;
    asked.push(args);
    return { kind: "Refused", reason: { kind: "Trigger", table: "public.orders", trigger: "public.orders_audit" } };
  });
  expect(asked).toEqual([{ tabId: "q1", schema: "public", table: "orders" }]);
  expect(screen.getByTestId("edit-refusal").textContent).toMatch(/^Edits are unavailable here: public.orders has an enabled trigger/);
  expect(screen.queryByTestId("add-row")).toBeNull();
});

it("marks a join's result Read-only, with the reason as its tooltip and description", async () => {
  await showQuery("Postgres");
  expect(screen.queryByTestId("read-only")).toBeNull();
  const catalog = shopCatalog("c1", 1);
  const run = tableRun("q1", catalog, "public", "orders", { kind: "Statement" }, 2);
  const customers = catalog.schemas[1]!.relations.find((r) => r.name === "customers")!;
  const email: ColumnMeta = { name: "email", type: { display: "text", category: "Text" }, source: { tableOid: customers.oid, attnum: 2 } };
  const output = run.kind === "Finished" ? run.statements[0]!.outputs[0]! : null;
  if (output?.kind !== "Rows" || run.kind !== "Finished") throw new Error("tableRun gives one Rows output");
  const joined = {
    ...run,
    statements: [{ ...run.statements[0]!, outputs: [{ ...output, result: { ...output.result, columns: [...output.result.columns, email] } }] }],
  };
  await act(async () => useLive.setState({ consoles: { q1: { ...idleConsole("q1", db), run: joined } } }));
  const label = screen.getByTestId("read-only");
  expect(label.title).toBe("Read-only: this result joins 2 tables.");
  expect(document.getElementById(label.getAttribute("aria-describedby")!)?.textContent).toBe("Read-only: this result joins 2 tables.");
  expect(screen.queryByTestId("add-row")).toBeNull();
});
