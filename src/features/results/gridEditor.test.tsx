// @vitest-environment jsdom
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterAll, afterEach, beforeAll, beforeEach, expect, it, vi } from "vitest";
import type { ResultMeta, RowPage } from "../../ipc/bindings";
import { dropRowCaches } from "../../state/rowCache";
import { NO_EDITS, useTableEdits } from "../../state/tableEdits";
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
