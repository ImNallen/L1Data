// @vitest-environment jsdom
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterAll, afterEach, beforeAll, beforeEach, expect, it, vi } from "vitest";
import { commands, type ConsoleState, type ResultMeta, type RowPage } from "../../ipc/bindings";
import { useLive } from "../../state/live";
import { dropRowCaches } from "../../state/rowCache";
import { useWorkspace, type Tab } from "../../state/workspace";
import { ResultsPane } from "./ResultsPane";
import { cellsState } from "../../testing/notebooks";

const TAB = "pager-test";
const db = { connectionId: "c1", database: "shop" };
const tab: Tab = { id: TAB, title: "Query", console: { db, mode: "AutoCommit" }, table: null, file: null };

const meta = (rowCount: number, index = 0, runId = 1): ResultMeta => ({
  databaseKind: "Postgres",
  ref: { tabId: TAB, runId, index },
  db,
  columns: [
    { name: "id", type: { display: "integer", category: "Integer" }, source: null },
    { name: "note", type: { display: "text", category: "Text" }, source: null },
  ],
  rowCount,
  rowsSeen: rowCount,
  truncated: null,
  elapsedMs: 5,
  allRows: { kind: "Statement" },
});

function showRun(rowCounts: number[], runId = 1) {
  const outputs = rowCounts.map((rowCount, index) => ({ kind: "Rows" as const, result: meta(rowCount, index, runId), state: "Complete" as const }));
  const state: ConsoleState = {
    tabId: TAB,
    rev: runId,
    config: { ...db, schema: null, mode: "AutoCommit" },
    session: { kind: "None" },
    txn: "Idle",
    txnDepth: null,
    work: null,
    export: null,
    import: null,
    run: { kind: "Finished", runId, elapsedMs: 5, outcome: "Ok", statements: [{ index: 0, range: { from: 0, to: 1 }, status: { kind: "Finished" }, outputs }] },
  };
  useLive.setState({ consoles: { [TAB]: state } });
}

const commandsSeen: string[] = [];
const pagesLoaded: { start: number; len: number }[] = [];
let cutCells: [number, number][] = [];

async function renderPane(rowCounts: number[], rowsPerPage: number) {
  useWorkspace.setState({ settings: { ...useWorkspace.getState().settings, tablePageSize: rowsPerPage } });
  showRun(rowCounts);
  await act(async () => {
    render(<ResultsPane tabId={TAB} />);
  });
}

const pageLabel = () => screen.getByTestId("result-page").textContent;
const go = (name: "First page" | "Previous page" | "Next page" | "Last page") => fireEvent.click(screen.getByRole("button", { name }));
const rowNumber = async (index: number) => (await screen.findByTestId(`grid-rownum-${index}`)).textContent;
const grid = () => screen.getByTestId("result-grid");

beforeAll(() => {
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockReturnValue(800);
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockReturnValue(600);
  vi.spyOn(HTMLElement.prototype, "scrollHeight", "get").mockReturnValue(1_000_000);
  vi.spyOn(HTMLElement.prototype, "scrollWidth", "get").mockReturnValue(800);
  Object.defineProperty(HTMLElement.prototype, "scrollTo", {
    configurable: true,
    value: function (this: HTMLElement, options: ScrollToOptions) {
      if (typeof options !== "object") return;
      if (options.top !== undefined) this.scrollTop = options.top;
      if (options.left !== undefined) this.scrollLeft = options.left;
      queueMicrotask(() => this.dispatchEvent(new Event("scroll")));
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

beforeEach(() => {
  commandsSeen.length = 0;
  pagesLoaded.length = 0;
  cutCells = [];
  mockIPC((cmd, args) => {
    commandsSeen.push(cmd);
    if (cmd === "result_page") {
      const { start, len } = args as { start: number; len: number };
      pagesLoaded.push({ start, len });
      const rows = Array.from({ length: len }, (_, i) => [String(start + i), `row ${start + i}`]);
      return { start, rows, cut: cutCells.filter(([row]) => row >= start && row < start + len) } satisfies RowPage;
    }
    if (cmd === "result_aggregate") return { kind: "Counts", count: 2, nonNull: 2 };
    if (cmd === "result_cell") return `full value of row ${(args as { row: number }).row}`;
    if (cmd === "settings_set") return null;
    throw new Error(`Unexpected IPC command: ${cmd}`);
  });
  useWorkspace.setState(cellsState([tab]));
});

afterEach(() => {
  cleanup();
  clearMocks();
  dropRowCaches(TAB);
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
});

afterAll(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

it("shows no pager for a result that fits the smallest page, and one for a row more", async () => {
  await renderPane([10], 10);
  expect(screen.queryByTestId("result-pager")).toBeNull();
  expect(await rowNumber(9)).toBe("10");
  cleanup();

  await renderPane([11], 10);
  expect(pageLabel()).toContain("1–10 of 11");
  expect(screen.queryByTestId("grid-rownum-10")).toBeNull();
});

it("pages through the stored rows with absolute row numbers, without running anything", async () => {
  await renderPane([2500], 500);
  expect(pageLabel()).toContain("1–500 of 2,500");
  expect(screen.getByRole("button", { name: "First page" }).hasAttribute("disabled")).toBe(true);

  go("Next page");
  expect(pageLabel()).toContain("501–1,000 of 2,500");
  expect(await rowNumber(0)).toBe("501");
  expect(await screen.findByText("row 500")).toBeTruthy();
  expect(pagesLoaded).toContainEqual({ start: 512, len: 256 });
  expect(grid().getAttribute("data-row-count")).toBe("2500");

  go("Previous page");
  expect(await rowNumber(0)).toBe("1");
  expect(pageLabel()).toContain("1–500 of 2,500");
  expect(new Set(commandsSeen)).toEqual(new Set(["result_page"]));
});

it("goes to the last and first pages, and loads the last page at its stored length", async () => {
  await renderPane([2100], 500);
  go("Last page");
  expect(pageLabel()).toContain("2,001–2,100 of 2,100");
  expect(await rowNumber(0)).toBe("2001");
  await waitFor(() => expect(pagesLoaded).toContainEqual({ start: 2048, len: 52 }));
  expect(screen.getByRole("button", { name: "Next page" }).hasAttribute("disabled")).toBe(true);
  expect(screen.getByRole("button", { name: "Last page" }).hasAttribute("disabled")).toBe(true);

  go("First page");
  expect(await rowNumber(0)).toBe("1");
});

it("widens the row number column and numbers rows for assistive technology across the whole result", async () => {
  await renderPane([200_000], 500);
  go("Last page");
  expect(await rowNumber(0)).toBe("199501");
  expect(parseFloat(screen.getByTestId("grid-rownum-0").style.width)).toBeCloseTo(6 * useWorkspace.getState().settings.codeFont.size * 0.6 + 14);
  expect(grid().getAttribute("aria-rowcount")).toBe("200001");
  expect(screen.getByTestId("grid-row-0").getAttribute("aria-rowindex")).toBe("199502");
});

it("returns to the first page for a new run and for another result of a batch", async () => {
  await renderPane([2500, 2500], 500);
  go("Next page");
  expect(pageLabel()).toContain("501–1,000");

  act(() => useWorkspace.getState().setActiveResult(TAB, 0));
  expect(pageLabel()).toContain("1–500");
  act(() => useWorkspace.getState().setActiveResult(TAB, 1));
  expect(pageLabel()).toContain("1–500");
  go("Last page");
  expect(pageLabel()).toContain("2,001–2,500");

  act(() => showRun([2500, 2500], 2));
  expect(pageLabel()).toContain("1–500");
  expect(await rowNumber(0)).toBe("1");
});

it("stays on its page while a streaming result grows", async () => {
  await renderPane([2500], 500);
  go("Last page");
  expect(pageLabel()).toContain("2,001–2,500 of 2,500");
  expect(screen.getByRole("button", { name: "Next page" }).hasAttribute("disabled")).toBe(true);

  act(() => {
    const state = useLive.getState().consoles[TAB]!;
    useLive.setState({
      consoles: {
        [TAB]: {
          ...state,
          run: {
            kind: "Running",
            runId: 1,
            startedAtMs: Date.now(),
            statements: [
              { index: 0, range: { from: 0, to: 1 }, status: { kind: "Running" }, outputs: [{ kind: "Rows", result: meta(2600), state: "Streaming" }] },
            ],
          },
        } as ConsoleState,
      },
    });
  });
  expect(pageLabel()).toContain("2,001–2,500 of 2,600");
  expect(await rowNumber(0)).toBe("2001");
  expect(screen.getByRole("button", { name: "Next page" }).hasAttribute("disabled")).toBe(false);
});

it("keeps the first row of the page on screen when the page size changes, and saves the size", async () => {
  const user = userEvent.setup();
  await renderPane([2500], 100);
  go("Next page");
  expect(await rowNumber(0)).toBe("101");

  await user.click(screen.getByTestId("result-page"));
  await user.click(await screen.findByTestId("page-size-500"));

  expect(pageLabel()).toContain("1–500 of 2,500");
  expect(useWorkspace.getState().settings.tablePageSize).toBe(500);
  await waitFor(() => expect(commandsSeen).toContain("settings_set"));
  expect(await rowNumber(100)).toBe("101");
  expect(commandsSeen).not.toContain("console_run");
});

it("keeps the same rows when the page size changes outside this pager", async () => {
  await renderPane([2500], 100);
  go("Next page");
  go("Next page");
  expect(pageLabel()).toContain("201–300");
  act(() => useWorkspace.setState({ settings: { ...useWorkspace.getState().settings, tablePageSize: 250 } }));
  expect(pageLabel()).toContain("1–250");
  expect(await rowNumber(200)).toBe("201");
});

it("clears the selection and scrolls to the top on another page, and keeps Find open", async () => {
  await renderPane([2500], 500);
  fireEvent.click(screen.getByText("Find in results"));
  const cell = await screen.findByTestId("grid-cell-0-0");
  fireEvent.mouseDown(cell, { button: 0 });
  await waitFor(() => expect(cell.getAttribute("aria-selected")).toBe("true"));
  grid().scrollTop = 2400;
  fireEvent.scroll(grid());

  go("Next page");
  expect(grid().scrollTop).toBe(0);
  expect((await screen.findByTestId("grid-cell-0-0")).getAttribute("aria-selected")).toBe("false");
  expect(screen.getByLabelText("Find in results")).toBeTruthy();
});

it("moves to the page of a Find match, selects the cell, and searches on from its absolute row", async () => {
  const search = vi.spyOn(commands, "resultFind").mockResolvedValue({
    status: "ok",
    data: { count: 2, rowCount: 2500, target: { cell: { row: 2300, col: 1 }, ordinal: 1 } },
  });
  await renderPane([2500], 500);
  fireEvent.click(screen.getByText("Find in results"));
  const input = screen.getByLabelText("Find in results");
  await waitFor(() => expect(document.activeElement).toBe(input));
  fireEvent.change(input, { target: { value: "elsewhere" } });
  await screen.findByText("1 of 2 matching cells");

  expect(pageLabel()).toContain("2,001–2,500 of 2,500");
  const found = await screen.findByTestId("grid-cell-300-1");
  expect(found.getAttribute("aria-selected")).toBe("true");
  expect(found.getAttribute("data-find-match")).toBe("true");
  expect(grid().getAttribute("aria-activedescendant")).toContain("300-1");
  expect(await rowNumber(300)).toBe("2301");
  expect(document.activeElement).toBe(input);
  await new Promise((done) => setTimeout(done, 500));
  expect(search).toHaveBeenCalledTimes(1);

  search.mockResolvedValue({ status: "ok", data: { count: 2, rowCount: 2500, target: { cell: { row: 3, col: 0 }, ordinal: 2 } } });
  fireEvent.keyDown(input, { key: "Enter" });
  await screen.findByText("2 of 2 matching cells");
  expect(search).toHaveBeenLastCalledWith(meta(2500).ref, "elsewhere", { kind: "Next", anchor: { row: 2300, col: 1 } });
  expect(pageLabel()).toContain("1–500 of 2,500");
  expect((await screen.findByTestId("grid-cell-3-0")).getAttribute("aria-selected")).toBe("true");
  search.mockRestore();
});

it("does not mark the match's window position on a page that does not hold it", async () => {
  const search = vi.spyOn(commands, "resultFind").mockResolvedValue({
    status: "ok",
    data: { count: 1, rowCount: 2500, target: { cell: { row: 503, col: 1 }, ordinal: 1 } },
  });
  await renderPane([2500], 500);
  fireEvent.click(screen.getByText("Find in results"));
  const input = screen.getByLabelText("Find in results");
  await waitFor(() => expect(document.activeElement).toBe(input));
  fireEvent.change(input, { target: { value: "elsewhere" } });
  await screen.findByText("1 of 1 matching cells");
  expect(pageLabel()).toContain("501–1,000");
  expect((await screen.findByTestId("grid-cell-3-1")).getAttribute("data-find-match")).toBe("true");

  go("First page");
  expect(await rowNumber(0)).toBe("1");
  expect((await screen.findByTestId("grid-cell-3-1")).hasAttribute("data-find-match")).toBe(false);
  search.mockRestore();
});

it("asks the engine about a cut cell's absolute row", async () => {
  cutCells = [[500, 1]];
  const search = vi.spyOn(commands, "resultFind").mockResolvedValue({ status: "ok", data: { count: 0, rowCount: 2500, target: null } });
  const matches = vi.spyOn(commands, "resultCellMatches").mockResolvedValue({ status: "ok", data: true });
  await renderPane([2500], 500);
  go("Next page");
  await screen.findByTestId("grid-cut-0-1");
  fireEvent.click(screen.getByText("Find in results"));
  fireEvent.change(screen.getByLabelText("Find in results"), { target: { value: "suffix" } });
  await waitFor(() => expect(matches).toHaveBeenCalledWith(meta(2500).ref, { row: 500, col: 1 }, "suffix"));
  await waitFor(() => expect(screen.getByTestId("grid-cell-0-1").getAttribute("data-find-match")).toBe("true"));
  search.mockRestore();
  matches.mockRestore();
});

it("totals the absolute rows of a selection, and select-all covers the current page", async () => {
  const aggregate = vi.spyOn(commands, "resultAggregate").mockResolvedValue({ status: "ok", data: { kind: "Counts", count: 2, nonNull: 2 } });
  await renderPane([2500], 500);
  go("Next page");
  await screen.findByText("row 500");
  fireEvent.mouseDown(screen.getByTestId("grid-cell-0-0"), { button: 0 });
  fireEvent.mouseUp(window);
  fireEvent.keyDown(grid(), { key: "ArrowDown", shiftKey: true });
  await waitFor(() => expect(aggregate).toHaveBeenLastCalledWith(meta(2500).ref, { start: 500, end: 501 }, { start: 0, end: 0 }));

  fireEvent.keyDown(grid(), { key: "a", metaKey: true });
  await waitFor(() => expect(aggregate).toHaveBeenLastCalledWith(meta(2500).ref, { start: 500, end: 999 }, { start: 0, end: 1 }));
  aggregate.mockRestore();
});

it("opens the record view and the cell viewer at the absolute row", async () => {
  await renderPane([2500], 500);
  go("Next page");
  await screen.findByText("row 502");
  fireEvent.mouseDown(screen.getByTestId("grid-cell-2-1"), { button: 0 });

  fireEvent.keyDown(grid(), { key: "i", metaKey: true });
  expect(useWorkspace.getState().overlay).toMatchObject({ kind: "record", row: 502 });
  act(() => useWorkspace.getState().close());

  fireEvent.keyDown(grid(), { key: "Enter" });
  await waitFor(() => expect(useWorkspace.getState().overlay).toEqual({ kind: "cell", title: "note · row 503", value: "row 502" }));
});

it("opens the full value of a cut cell from its absolute row", async () => {
  cutCells = [[500, 1]];
  await renderPane([2500], 500);
  go("Next page");
  fireEvent.click(await screen.findByTestId("grid-cut-0-1"));
  await waitFor(() => expect(useWorkspace.getState().overlay).toEqual({ kind: "cell", title: "note · row 501", value: "full value of row 500" }));
});

it("copies the rows of the page, not the rows at the same place in the result", async () => {
  const writeText = vi.fn().mockResolvedValue(undefined);
  Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
  await renderPane([2500], 500);
  go("Next page");
  await screen.findByText("row 500");
  fireEvent.mouseDown(screen.getByTestId("grid-cell-0-1"), { button: 0 });
  fireEvent.keyDown(grid(), { key: "c", metaKey: true });
  await waitFor(() => expect(writeText).toHaveBeenCalledWith("row 500"));
});
