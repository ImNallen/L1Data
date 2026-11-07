// @vitest-environment jsdom
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ConsoleState, ResultMeta, TableScript } from "../../ipc/bindings";
import { useLive } from "../../state/live";
import { dropRowCaches } from "../../state/rowCache";
import { EMPTY_QUERY, useWorkspace, type TableTab } from "../../state/workspace";
import { dropEditor, seedEditor } from "../editor/editorStates";
import { browseTemplate } from "../editor/runActions";
import { ResultsPane } from "./ResultsPane";

const db = { connectionId: "c1", database: "shop" };
const tab: TableTab = {
  id: "t1",
  title: "customers",
  console: { db, mode: "AutoCommit" },
  table: { schema: "public", table: "customers", page: 1, query: EMPTY_QUERY },
  file: null,
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
    tabs: { [tab.id]: tab },
    order: [tab.id],
    active: tab.id,
    settings: { ...useWorkspace.getState().settings, tablePageSize: 1000, maxRows: 100 },
  });
  await browseTemplate(db, tab.table.schema, tab.table.table);
  seedEditor(tab.id, script.text);
});

afterEach(() => {
  cleanup();
  clearMocks();
  dropRowCaches(tab.id);
  dropEditor(tab.id);
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
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
