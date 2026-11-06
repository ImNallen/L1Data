// @vitest-environment jsdom
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterAll, afterEach, beforeAll, expect, it, vi } from "vitest";
import { commands, type ResultFind, type ResultMeta, type ResultRef } from "../../ipc/bindings";
import { NO_EDITS, setCell, useTableEdits } from "../../state/tableEdits";
import { dropRowCaches } from "../../state/rowCache";
import { ResultGrid } from "./ResultGrid";
import { storedValueMatches, useResultFind } from "./useResultFind";

const meta: ResultMeta = {
  databaseKind: "Postgres",
  ref: { tabId: "find-test", runId: 1, index: 0 },
  db: { connectionId: "c1", database: "shop" },
  columns: [{ name: "value", type: { display: "text", category: "Text" }, source: null }],
  rowCount: 10_000,
  rowsSeen: 10_000,
  truncated: "RowLimit",
  elapsedMs: 1,
  allRows: { kind: "Statement" },
};
const result = (row: number, ordinal = 1): ResultFind => ({ count: 2, rowCount: 10_000, target: { cell: { row, col: 0 }, ordinal } });

beforeAll(() => {
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockReturnValue(800);
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockReturnValue(600);
  vi.spyOn(HTMLElement.prototype, "scrollHeight", "get").mockReturnValue(300_000);
  vi.spyOn(HTMLElement.prototype, "scrollWidth", "get").mockReturnValue(800);
  Object.defineProperty(HTMLElement.prototype, "scrollTo", {
    configurable: true,
    value: function (this: HTMLElement, options: ScrollToOptions) {
      if (typeof options !== "object") return;
      if (options.top !== undefined) this.scrollTop = options.top;
      if (options.left !== undefined) this.scrollLeft = options.left;
      this.dispatchEvent(new Event("scroll"));
    },
  });
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
afterEach(() => {
  cleanup();
  clearMocks();
  dropRowCaches("find-test");
  useTableEdits.setState(useTableEdits.getInitialState(), true);
  vi.clearAllMocks();
});
afterAll(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

it("opens scoped find, navigates offscreen without stealing input focus, and restores grid focus", async () => {
  mockIPC((cmd) => {
    if (cmd === "result_page") return { start: 0, rows: [["hit"]], cut: [] };
    throw new Error(cmd);
  });
  const search = vi.spyOn(commands, "resultFind").mockResolvedValue({ status: "ok", data: result(9000) });
  render(<ResultGrid meta={meta} done query={null} editing={null} />);
  const grid = screen.getByRole("grid");
  grid.focus();
  fireEvent.keyDown(grid, { key: "f", ctrlKey: true });
  const input = screen.getByRole("textbox", { name: "Find in results" });
  await waitFor(() => expect(document.activeElement).toBe(input));
  expect(screen.getByText(/Current result.*Result truncated/)).toBeTruthy();
  fireEvent.change(input, { target: { value: "hit" } });
  await screen.findByText("1 of 2 matching cells");
  expect(grid.getAttribute("aria-activedescendant")).toContain("9000-0");
  expect(document.activeElement).toBe(input);
  search.mockResolvedValue({ status: "ok", data: result(0, 2) });
  fireEvent.keyDown(input, { key: "Enter" });
  await screen.findByText("2 of 2 matching cells");
  expect(search).toHaveBeenLastCalledWith(meta.ref, "hit", { kind: "Next", anchor: { row: 9000, col: 0 } });
  search.mockResolvedValue({ status: "ok", data: result(9000) });
  fireEvent.keyDown(input, { key: "Enter", shiftKey: true });
  await screen.findByText("1 of 2 matching cells");
  expect(search).toHaveBeenLastCalledWith(meta.ref, "hit", { kind: "Previous", anchor: { row: 0, col: 0 } });
  const shortcut = new KeyboardEvent("keydown", { key: "p", ctrlKey: true, bubbles: true, cancelable: true });
  input.dispatchEvent(shortcut);
  expect(shortcut.defaultPrevented).toBe(false);
  const commandsShortcut = new KeyboardEvent("keydown", { key: "P", ctrlKey: true, shiftKey: true, bubbles: true, cancelable: true });
  input.dispatchEvent(commandsShortcut);
  expect(commandsShortcut.defaultPrevented).toBe(false);
  fireEvent.keyDown(input, { key: "Escape" });
  expect(screen.queryByRole("textbox")).toBeNull();
  expect(document.activeElement).toBe(grid);
  search.mockRestore();
});

function Harness({
  rows = 10_000,
  reference = meta.ref,
  select,
}: {
  rows?: number;
  reference?: ResultRef;
  select: (cell: { row: number; col: number }) => void;
}) {
  const find = useResultFind(reference, rows, select);
  return (
    <>
      <button onClick={find.show}>Open</button>
      <button onClick={find.close}>Close</button>
      <input aria-label="query" value={find.query} onChange={(e) => find.change(e.target.value)} />
      <span>{find.state.kind === "failed" ? find.state.message : find.state.kind}</span>
    </>
  );
}

it("bounds obsolete work and drops stale replies after query changes and close", async () => {
  let resolve: ((value: Awaited<ReturnType<typeof commands.resultFind>>) => void) | undefined;
  const search = vi
    .spyOn(commands, "resultFind")
    .mockImplementationOnce(
      () =>
        new Promise((done) => {
          resolve = done;
        }),
    )
    .mockResolvedValue({ status: "ok", data: result(3) });
  const select = vi.fn();
  render(<Harness select={select} />);
  fireEvent.click(screen.getByText("Open"));
  fireEvent.change(screen.getByLabelText("query"), { target: { value: "old" } });
  await waitFor(() => expect(search).toHaveBeenCalledTimes(1));
  fireEvent.change(screen.getByLabelText("query"), { target: { value: "new" } });
  await new Promise((done) => setTimeout(done, 250));
  expect(search).toHaveBeenCalledTimes(1);
  resolve?.({ status: "ok", data: result(999) });
  await waitFor(() => expect(select).toHaveBeenCalledWith({ row: 3, col: 0 }));
  expect(select).toHaveBeenCalledTimes(1);
  fireEvent.click(screen.getByText("Close"));
  expect(screen.getByText("closed")).toBeTruthy();
  search.mockRestore();
});

it("reports engine errors", async () => {
  const search = vi.spyOn(commands, "resultFind").mockResolvedValue({ status: "error", error: { kind: "Gone" } });
  const select = vi.fn();
  render(<Harness select={select} />);
  fireEvent.click(screen.getByText("Open"));
  await screen.findByText("This result has been replaced.");
  expect(select).not.toHaveBeenCalled();
  search.mockRestore();
});

it("matches stored Unicode text literally and excludes nulls", () => {
  expect(storedValueMatches("Éİ尾", "éi̇")).toBe(true);
  expect(storedValueMatches(null, "NULL")).toBe(false);
  expect(storedValueMatches("abc", "")).toBe(false);
  expect(storedValueMatches("a.b", ".")).toBe(true);
});

it("highlights every visible cut-cell suffix without pulling full values into the grid", async () => {
  mockIPC((cmd) => {
    if (cmd === "result_page")
      return {
        start: 0,
        rows: [["prefix"], ["prefix"]],
        cut: [
          [0, 0],
          [1, 0],
        ],
      };
    throw new Error(cmd);
  });
  const search = vi.spyOn(commands, "resultFind").mockResolvedValue({ status: "ok", data: { ...result(0), rowCount: 2 } });
  const matches = vi.spyOn(commands, "resultCellMatches").mockResolvedValue({ status: "ok", data: true });
  render(<ResultGrid meta={{ ...meta, rowCount: 2 }} done query={null} editing={null} />);
  fireEvent.click(screen.getByText("Find in results"));
  fireEvent.change(screen.getByLabelText("Find in results"), { target: { value: "suffix" } });
  await screen.findByText("1 of 2 matching cells");
  await waitFor(() => expect(screen.getByTestId("grid-cell-1-0").getAttribute("data-find-match")).toBe("true"));
  expect(matches).toHaveBeenCalledWith(meta.ref, { row: 1, col: 0 }, "suffix");
  search.mockRestore();
  matches.mockRestore();
});

it("does not move the grid after focus leaves Find while a search is pending", async () => {
  mockIPC(() => ({ start: 0, rows: [["hit"]], cut: [] }));
  let resolve: ((value: Awaited<ReturnType<typeof commands.resultFind>>) => void) | undefined;
  const search = vi.spyOn(commands, "resultFind").mockImplementation(
    () =>
      new Promise((done) => {
        resolve = done;
      }),
  );
  render(<ResultGrid meta={meta} done query={null} editing={null} />);
  fireEvent.click(screen.getByText("Find in results"));
  const input = screen.getByLabelText("Find in results");
  fireEvent.change(input, { target: { value: "hit" } });
  await waitFor(() => expect(search).toHaveBeenCalledTimes(1));
  const grid = screen.getByRole("grid");
  grid.focus();
  resolve?.({ status: "ok", data: result(9000) });
  await screen.findByText("1 of 2 matching cells");
  expect(grid.getAttribute("aria-activedescendant")).toBeNull();
  expect(document.activeElement).toBe(grid);
  search.mockRestore();
});

it("labels a table page without searching other pages", () => {
  mockIPC(() => ({ start: 0, rows: [], cut: [] }));
  render(<ResultGrid meta={meta} done editing={null} query={{ sort: [], filtered: new Set(), onSort() {}, onFilterColumn() {}, onFilterValue() {} }} />);
  expect(screen.getByText(/Current table page/)).toBeTruthy();
});

it("never publishes a pending reply after close or unmount", async () => {
  let resolve: ((value: Awaited<ReturnType<typeof commands.resultFind>>) => void) | undefined;
  const search = vi.spyOn(commands, "resultFind").mockImplementation(
    () =>
      new Promise((done) => {
        resolve = done;
      }),
  );
  const select = vi.fn();
  const view = render(<Harness select={select} />);
  fireEvent.click(screen.getByText("Open"));
  await waitFor(() => expect(search).toHaveBeenCalledTimes(1));
  fireEvent.click(screen.getByText("Close"));
  view.unmount();
  resolve?.({ status: "ok", data: result(999) });
  await new Promise((done) => setTimeout(done, 0));
  expect(select).not.toHaveBeenCalled();
  search.mockRestore();
});

it("publishes a streaming snapshot before refreshing the new row bound", async () => {
  let resolve: ((value: Awaited<ReturnType<typeof commands.resultFind>>) => void) | undefined;
  const search = vi
    .spyOn(commands, "resultFind")
    .mockImplementationOnce(
      () =>
        new Promise((done) => {
          resolve = done;
        }),
    )
    .mockResolvedValue({ status: "ok", data: { ...result(2), rowCount: 10_001 } });
  const select = vi.fn();
  const view = render(<Harness select={select} />);
  fireEvent.click(screen.getByText("Open"));
  await waitFor(() => expect(search).toHaveBeenCalledTimes(1));
  view.rerender(<Harness rows={10_001} select={select} />);
  resolve?.({ status: "ok", data: result(1) });
  await waitFor(() => expect(select).toHaveBeenCalledWith({ row: 1, col: 0 }));
  await waitFor(() => expect(select).toHaveBeenCalledWith({ row: 2, col: 0 }));
  search.mockRestore();
});

it("highlights a full-value suffix target but never its staged display value", async () => {
  mockIPC(() => ({ start: 0, rows: [["prefix"]], cut: [[0, 0]] }));
  const search = vi.spyOn(commands, "resultFind").mockResolvedValue({ status: "ok", data: { ...result(0), rowCount: 1 } });
  render(<ResultGrid meta={{ ...meta, rowCount: 1 }} done query={null} editing={{ pkColumns: ["value"], writableColumns: new Set(["value"]) }} />);
  fireEvent.click(screen.getByText("Find in results"));
  fireEvent.change(screen.getByLabelText("Find in results"), { target: { value: "suffix" } });
  await screen.findByText("1 of 2 matching cells");
  expect(screen.getByTestId("grid-cell-0-0").getAttribute("data-find-match")).toBe("true");
  const edits = setCell(NO_EDITS, [{ column: "value", value: "prefix" }], "value", "suffix draft", "prefix");
  useTableEdits.setState({ byTab: { "find-test": edits } });
  await screen.findByText("suffix draft");
  expect(screen.getByTestId("grid-cell-0-0").hasAttribute("data-find-match")).toBe(false);
  search.mockRestore();
});

it("drops the old result reply when the result identity changes", async () => {
  let resolve: ((value: Awaited<ReturnType<typeof commands.resultFind>>) => void) | undefined;
  const search = vi
    .spyOn(commands, "resultFind")
    .mockImplementationOnce(
      () =>
        new Promise((done) => {
          resolve = done;
        }),
    )
    .mockResolvedValue({ status: "ok", data: result(2) });
  const select = vi.fn();
  const view = render(<Harness select={select} />);
  fireEvent.click(screen.getByText("Open"));
  await waitFor(() => expect(search).toHaveBeenCalledTimes(1));
  const reference = { ...meta.ref, tabId: "other-tab", runId: 2 };
  view.rerender(<Harness reference={reference} select={select} />);
  resolve?.({ status: "ok", data: result(999) });
  await waitFor(() => expect(select).toHaveBeenCalledWith({ row: 2, col: 0 }));
  expect(select).toHaveBeenCalledTimes(1);
  expect(search).toHaveBeenLastCalledWith(reference, "", { kind: "First" });
  search.mockRestore();
});
