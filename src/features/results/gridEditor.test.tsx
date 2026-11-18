// @vitest-environment jsdom
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterAll, afterEach, beforeAll, beforeEach, expect, it, vi } from "vitest";
import type { ConsoleState, ResultMeta, RowPage } from "../../ipc/bindings";
import { useLive } from "../../state/live";
import { dropRowCaches } from "../../state/rowCache";
import { NO_EDITS, stagingSettled, toChanges, useTableEdits } from "../../state/tableEdits";
import { useWorkspace } from "../../state/workspace";
import { ResultGrid, type GridEditing } from "./ResultGrid";

const meta: ResultMeta = {
  databaseKind: "Postgres",
  ref: { tabId: "t1", runId: 1, index: 0 },
  db: { connectionId: "c1", database: "shop" },
  columns: [
    { name: "id", type: { display: "bigint", category: "Integer" }, source: { tableOid: 16_406, attnum: 1 } },
    { name: "status", type: { display: "text", category: "Text" }, source: { tableOid: 16_406, attnum: 3 } },
  ],
  rowCount: 1,
  rowsSeen: 1,
  truncated: null,
  elapsedMs: 1,
  allRows: { kind: "Table" },
};
const editing: GridEditing = { pkColumns: ["id"], writableColumns: new Set(["status"]) };

beforeAll(() => {
  // jsdom lays nothing out, and the virtualizer renders no rows into a zero-size viewport.
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockReturnValue(800);
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockReturnValue(600);
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      unobserve() {}
      disconnect() {}
    },
  );
});

afterAll(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

beforeEach(() => {
  mockIPC((cmd): RowPage => {
    if (cmd === "result_page") return { start: 0, rows: [["1", "paid"]], cut: [] };
    throw "not served in this test";
  });
});

afterEach(() => {
  cleanup();
  clearMocks();
  dropRowCaches("t1");
  useTableEdits.setState(useTableEdits.getInitialState(), true);
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
});

const pending = () => useTableEdits.getState().byTab.t1 ?? NO_EDITS;

async function typeIntoStatus(user: ReturnType<typeof userEvent.setup>, text: string) {
  render(<ResultGrid meta={meta} done query={null} editing={editing} />);
  await user.dblClick(await screen.findByText("paid"));
  const editor = screen.getByTestId("grid-editor");
  await user.clear(editor);
  await user.type(editor, text);
}

it("drops the draft when Escape cancels the edit", async () => {
  const user = userEvent.setup();
  await typeIntoStatus(user, "shipped");
  await user.keyboard("{Escape}");
  expect(screen.queryByTestId("grid-editor")).toBeNull();
  expect(pending().updates).toEqual({});
  expect(screen.getByTestId("result-grid").getAttribute("data-pending")).toBe("0");
  expect(screen.getByText("paid")).toBeTruthy();
});

it("stages the draft when Enter commits the edit", async () => {
  const user = userEvent.setup();
  await typeIntoStatus(user, "shipped");
  await user.keyboard("{Enter}");
  expect(screen.queryByTestId("grid-editor")).toBeNull();
  expect(Object.values(pending().updates)).toEqual([{ key: [{ column: "id", value: "1" }], values: { status: "shipped" } }]);
  expect(screen.getByTestId("result-grid").getAttribute("data-pending")).toBe("1");
});

it("opens results find from a cell editor through its normal draft finish path", async () => {
  const user = userEvent.setup();
  await typeIntoStatus(user, "shipped");
  await user.keyboard("{Control>}f{/Control}");
  expect(screen.queryByTestId("grid-editor")).toBeNull();
  expect(Object.values(pending().updates)).toEqual([{ key: [{ column: "id", value: "1" }], values: { status: "shipped" } }]);
  const input = screen.getByRole("textbox", { name: "Find in results" });
  await vi.waitFor(() => expect(document.activeElement).toBe(input));
  expect(screen.getByText(/Saved\/retained values only/)).toBeTruthy();
});

it("holds Submit while a typed-over SQL Server cell loads its full original value", async () => {
  const user = userEvent.setup();
  let full!: (value: string) => void;
  mockIPC((cmd): RowPage | Promise<string> => {
    if (cmd === "result_page") return { start: 0, rows: [["1", "pai"]], cut: [[0, 1]] };
    if (cmd === "result_cell") return new Promise((resolve) => (full = resolve));
    throw "not served in this test";
  });
  render(<ResultGrid meta={{ ...meta, databaseKind: "SqlServer" }} done query={null} editing={{ ...editing, rowCheck: { kind: "Values" } }} />);
  await user.click(await screen.findByText("pai"));
  await user.keyboard("x{Enter}");
  expect(screen.queryByTestId("grid-editor")).toBeNull();
  let settled = false;
  const held = stagingSettled("t1").then(() => (settled = true));
  await vi.waitFor(() => expect(full).toBeTypeOf("function"));
  expect([settled, pending().updates]).toEqual([false, {}]);
  full("paid in full");
  await held;
  expect(toChanges(pending(), "dbo", "t", { kind: "Values" }).changes.updates).toEqual([
    { key: [{ column: "id", value: "1" }], set: [{ column: "status", value: "x" }], expect: [{ column: "status", value: "paid in full" }] },
  ]);
});

it("stops a running SQL Server Submit when Esc is pressed in the grid", async () => {
  const user = userEvent.setup();
  const cancels: unknown[] = [];
  mockIPC((cmd, args): RowPage | null => {
    if (cmd === "result_page") return { start: 0, rows: [["1", "paid"]], cut: [] };
    if (cmd !== "console_cancel") throw "not served in this test";
    cancels.push(args);
    return null;
  });
  render(<ResultGrid meta={{ ...meta, databaseKind: "SqlServer" }} done query={null} editing={editing} />);
  await user.click(await screen.findByText("paid"));
  await user.keyboard("{Escape}");
  expect(cancels).toEqual([]);
  useLive.setState({ consoles: { t1: { work: "Edit" } as ConsoleState } });
  await user.keyboard("{Escape}");
  expect(cancels).toEqual([{ tabId: "t1" }]);
});
