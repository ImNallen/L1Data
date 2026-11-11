// @vitest-environment jsdom
import { completionStatus } from "@codemirror/autocomplete";
import { EditorView } from "@codemirror/view";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ConsoleState, ResultMeta, TabId } from "../../ipc/bindings";
import { useLive } from "../../state/live";
import { dropRowCaches } from "../../state/rowCache";
import { activeId, useWorkspace, type Tab } from "../../state/workspace";
import { cellNavigation } from "../notebooks/cellNavigation";
import { cursorOf, dropEditor, editorStateOf, installEditorBehavior, moveCursor, seedEditor } from "./editorStates";
import { NotebookWorkspace } from "./NotebookWorkspace";
import { cellsState } from "../../testing/notebooks";

const IDS = ["a", "b"];
const db = { connectionId: "c1", database: "shop" };
const FIVE_LINES = "SELECT\n  id,\n  name\nFROM\n  customers";

const FILLED = 672;
const PAGER = 36;
const COMPACT = 320 + PAGER;
const EDITOR = 128;

let viewport = 858;
let notifyResize: () => void = () => {};

const tab = (id: TabId): Tab => ({ id, title: `Cell ${id}`, console: { db, mode: "AutoCommit" }, table: null });

function finished(id: TabId, rowCounts: number[]): ConsoleState {
  const outputs = rowCounts.map((rowCount, index) => {
    const result: ResultMeta = {
      databaseKind: "Postgres",
      ref: { tabId: id, runId: 1, index },
      db,
      columns: [{ name: "id", type: { display: "integer", category: "Integer" }, source: null }],
      rowCount,
      rowsSeen: rowCount,
      truncated: null,
      elapsedMs: 5,
      allRows: { kind: "Statement" },
    };
    return { kind: "Rows" as const, result, state: "Complete" as const };
  });
  return {
    tabId: id,
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
      statements: [{ index: 0, range: { from: 0, to: FIVE_LINES.length }, status: { kind: "Finished" }, outputs }],
    },
  };
}

function withRowCounts(counts: Record<TabId, number[]>) {
  useLive.setState({ consoles: Object.fromEntries(Object.entries(counts).map(([id, rowCounts]) => [id, finished(id, rowCounts)])) });
}

const cell = (id: TabId) => screen.getByTestId(`notebook-cell-${id}`);
const heightOf = (el: Element | null) => parseInt((el as HTMLElement).style.height, 10);
const editorHeight = (id: TabId) => heightOf(cell(id).querySelector(".notebook-editor"));
const resultsHeight = (id: TabId) => heightOf(cell(id).querySelector(".notebook-results"));
const resultsHeights = () => [resultsHeight("a"), resultsHeight("b")];
const afterClick = () => act(() => new Promise((resolve) => setTimeout(resolve, 0)));

async function renderNotebook() {
  await act(async () => {
    render(<NotebookWorkspace />);
  });
}

function resizeViewport(px: number) {
  viewport = px;
  act(() => notifyResize());
}

beforeEach(() => {
  viewport = 858;
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  vi.spyOn(Element.prototype, "clientHeight", "get").mockImplementation(function (this: Element) {
    return this.getAttribute("data-testid") === "notebook" ? viewport : 0;
  });
  Element.prototype.scrollIntoView = () => {};
  Element.prototype.getAnimations = () => [];
  Range.prototype.getClientRects = () => Object.assign([], { item: () => null });
  HTMLElement.prototype.setPointerCapture = () => {};
  vi.stubGlobal(
    "ResizeObserver",
    class {
      constructor(private callback: () => void) {}
      observe(el: Element) {
        if (el === screen.queryByTestId("notebook")) notifyResize = this.callback;
      }
      unobserve() {}
      disconnect() {}
    },
  );
  mockIPC((cmd) => {
    if (cmd === "result_page") return { start: 0, rows: [["1"]], cut: [] };
    throw new Error(`Unexpected IPC command: ${cmd}`);
  });
  useWorkspace.setState({
    ...cellsState([tab("a"), tab("b")]),
    settings: { ...useWorkspace.getState().settings, density: "Standard", codeFont: { ...useWorkspace.getState().settings.codeFont, size: 13 } },
  });
  for (const id of IDS) seedEditor(id, FIVE_LINES);
});

afterEach(() => {
  cleanup();
  clearMocks();
  for (const id of IDS) {
    dropRowCaches(id);
    dropEditor(id);
  }
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

it("lays out an inactive cell's SQL unwrapped, as its editor does", async () => {
  await renderNotebook();
  expect([cell("a").querySelector(".cm-content.cm-lineWrapping"), screen.getByTestId("cell-preview-b").querySelector(".cm-content.cm-lineWrapping")]).toEqual([
    null,
    null,
  ]);
});

it("steps the query height with the arrow keys and returns to automatic on double-click", async () => {
  await renderNotebook();
  const handle = screen.getByRole("separator", { name: "Resize query in Cell a" });
  expect(editorHeight("a")).toBe(EDITOR);

  fireEvent.keyDown(handle, { key: "ArrowDown" });
  expect(editorHeight("a")).toBe(EDITOR + 8);
  expect(handle.getAttribute("aria-valuenow")).toBe(String(EDITOR + 8));
  fireEvent.keyDown(handle, { key: "ArrowUp" });
  fireEvent.keyDown(handle, { key: "ArrowUp" });
  expect(editorHeight("a")).toBe(EDITOR - 8);

  fireEvent.doubleClick(handle);
  expect(editorHeight("a")).toBe(EDITOR);
});

it("drags the handle under the query from the height on screen, and shrinks automatic results to match", async () => {
  withRowCounts({ a: [1000] });
  await renderNotebook();
  const handle = within(cell("a")).getByTestId("cell-editor-resize");

  fireEvent.pointerDown(handle, { button: 0, clientY: 100 });
  fireEvent.pointerMove(handle, { clientY: 130 });
  expect(editorHeight("a")).toBe(EDITOR + 30);
  expect(resultsHeight("a")).toBe(FILLED - 30);
  expect(handle.getAttribute("data-separator")).toBe("active");

  fireEvent.pointerUp(handle);
  fireEvent.pointerMove(handle, { clientY: 300 });
  expect(editorHeight("a")).toBe(EDITOR + 30);
  expect(handle.getAttribute("data-separator")).toBeNull();
});

it("keeps following the viewport after presses that never moved the pointer", async () => {
  withRowCounts({ a: [1000] });
  await renderNotebook();
  const handle = screen.getByRole("separator", { name: "Resize results in Cell a" });

  for (let click = 0; click < 2; click++) {
    fireEvent.pointerDown(handle, { button: 0, clientY: 100 });
    fireEvent.pointerUp(handle, { clientY: 100 });
  }
  resizeViewport(1058);

  expect(resultsHeight("a")).toBe(1000 - EDITOR);
});

it("stops a dragged result at the room left in the viewport and returns to automatic on double-click", async () => {
  withRowCounts({ a: [1000] });
  await renderNotebook();
  const handle = screen.getByRole("separator", { name: "Resize results in Cell a" });

  fireEvent.pointerDown(handle, { button: 0, clientY: 100 });
  fireEvent.pointerMove(handle, { clientY: 5000 });
  fireEvent.pointerUp(handle);
  expect(resultsHeight("a")).toBe(FILLED);
  resizeViewport(1058);
  expect(resultsHeight("a")).toBe(FILLED);
  resizeViewport(858);

  fireEvent.pointerDown(handle, { button: 0, clientY: 100 });
  fireEvent.pointerMove(handle, { clientY: 0 });
  fireEvent.pointerUp(handle);
  expect(resultsHeight("a")).toBe(FILLED - 100);
  fireEvent.keyDown(handle, { key: "ArrowDown" });
  expect(resultsHeight("a")).toBe(FILLED - 92);

  fireEvent.doubleClick(handle);
  expect(resultsHeight("a")).toBe(FILLED);
});

it("resizes the query from the handle under the cell while no results show", async () => {
  await renderNotebook();
  expect(screen.queryByTestId("cell-results-resize")).toBeNull();
  expect(within(cell("a")).queryByTestId("cell-editor-resize")).toBeNull();
  expect(screen.getAllByRole("separator", { name: "Resize query in Cell a" })).toHaveLength(1);
});

it("grows and shrinks the focused cell's results with the notebook", async () => {
  withRowCounts({ a: [1000], b: [1000] });
  await renderNotebook();
  expect(resultsHeights()).toEqual([FILLED, COMPACT]);

  resizeViewport(1058);
  expect(resultsHeights()).toEqual([1000 - EDITOR, COMPACT]);
});

it("gives a batch the space of its largest result", async () => {
  withRowCounts({ a: [2, 1000] });
  await renderNotebook();
  expect(resultsHeight("a")).toBe(FILLED);
});

it("sizes a paged query to one page and its pager, not to every stored row", async () => {
  useWorkspace.setState({ settings: { ...useWorkspace.getState().settings, tablePageSize: 10 } });
  withRowCounts({ a: [1000], b: [10] });
  await renderNotebook();
  expect(resultsHeights()).toEqual([80 + 10 * 24 + PAGER, 80 + 10 * 24]);
});

it("leaves a result that shows no pager no room for one", async () => {
  useWorkspace.setState({ settings: { ...useWorkspace.getState().settings, tablePageSize: 100 } });
  withRowCounts({ a: [10], b: [11] });
  await renderNotebook();
  expect(resultsHeights()).toEqual([80 + 10 * 24, 80 + 10 * 24 + PAGER]);
});

it("holds both cells' sizes through a press on an inactive cell until after the click", async () => {
  withRowCounts({ a: [1000], b: [1000] });
  await renderNotebook();
  const target = cell("b").querySelector(".notebook-results") as HTMLElement;

  fireEvent.pointerDown(target);
  fireEvent.mouseDown(target);
  expect(activeId(useWorkspace.getState())).toBe("b");
  expect(resultsHeights()).toEqual([FILLED, COMPACT]);

  fireEvent.mouseUp(target);
  fireEvent.pointerUp(target);
  fireEvent.click(target);
  expect(resultsHeights()).toEqual([FILLED, COMPACT]);

  await afterClick();
  expect(resultsHeights()).toEqual([COMPACT, FILLED]);
});

it("focuses the editor at the clicked spot on one click into an inactive cell's SQL", async () => {
  // jsdom has no layout, so a click's coordinates stand for a line and column.
  vi.spyOn(EditorView.prototype, "posAtCoords").mockImplementation(function (this: EditorView, { x, y }: { x: number; y: number }) {
    return this.state.doc.line(y).from + x;
  });
  await renderNotebook();
  const target = cell("b").querySelector(".cm-line") as HTMLElement;
  const at = { button: 0, clientX: 2, clientY: 3 };

  fireEvent.pointerDown(target, at);
  fireEvent.mouseDown(target, at);
  fireEvent.mouseUp(target, at);
  fireEvent.pointerUp(target, at);
  fireEvent.click(target, at);
  await afterClick();

  expect(activeId(useWorkspace.getState())).toBe("b");
  expect(document.activeElement).toBe(within(cell("b")).getByTestId("sql-editor").querySelector(".cm-content"));
  expect(cursorOf(editorStateOf("b"))).toEqual({ line: 3, col: 3 });
});

it("keeps a clicked cell in place while the cell above it shrinks, instead of snapping it into view", async () => {
  withRowCounts({ a: [1000], b: [1000] });
  await renderNotebook();
  const notebook = screen.getByTestId("notebook");
  let scrollTop = 1000;
  Object.defineProperty(notebook, "scrollTop", {
    get: () => scrollTop,
    set: (px: number) => {
      scrollTop = px;
    },
  });
  vi.spyOn(cell("b"), "getBoundingClientRect").mockImplementation(() => DOMRect.fromRect({ y: resultsHeight("a") + 828 - scrollTop, height: 300 }));
  const scroll = vi.spyOn(Element.prototype, "scrollIntoView").mockImplementation(() => {});
  const target = cell("b").querySelector(".notebook-results") as HTMLElement;
  expect(cell("b").getBoundingClientRect().top).toBe(500);

  fireEvent.pointerDown(target);
  fireEvent.mouseDown(target);
  fireEvent.pointerUp(target);
  await afterClick();

  expect(resultsHeights()).toEqual([COMPACT, FILLED]);
  expect(cell("b").getBoundingClientRect().top).toBe(500);
  expect(scroll).not.toHaveBeenCalled();
});

it("resizes straight away when a cell activates without a press", async () => {
  withRowCounts({ a: [1000], b: [1000] });
  await renderNotebook();
  await act(async () => {
    useWorkspace.getState().setActive("b");
  });
  expect(resultsHeights()).toEqual([COMPACT, FILLED]);
});

it("keeps the notebook still through a drag and reset of the focused cell, and follows the viewport after", async () => {
  withRowCounts({ a: [1000] });
  await renderNotebook();
  const scroll = vi.spyOn(Element.prototype, "scrollIntoView").mockImplementation(() => {});
  const handle = within(cell("a")).getByTestId("cell-editor-resize");

  fireEvent.pointerDown(handle, { button: 0, clientY: 100 });
  fireEvent.pointerMove(handle, { clientY: 160 });
  expect(editorHeight("a")).toBe(EDITOR + 60);
  fireEvent.pointerUp(handle, { clientY: 160 });
  fireEvent.doubleClick(handle);
  expect(editorHeight("a")).toBe(EDITOR);
  expect(scroll).not.toHaveBeenCalled();

  resizeViewport(1058);
  expect(scroll.mock.contexts).toEqual([cell("a")]);
});

it("holds the notebook's end under the pointer when a drag that shrank the last cell ends", async () => {
  withRowCounts({ a: [1000], b: [1000] });
  await renderNotebook();
  const notebook = screen.getByTestId("notebook");
  // jsdom has no layout, so the notebook's height is the last cell's results, the CSS drag slack, and a fixed rest.
  const scrollHeight = () => 1500 + resultsHeight("b") + (parseFloat(notebook.style.getPropertyValue("--notebook-drag-slack")) || 0);
  let scrollTop = 0;
  Object.defineProperty(notebook, "scrollHeight", { get: scrollHeight });
  Object.defineProperty(notebook, "scrollTop", {
    get: () => (scrollTop = Math.min(scrollTop, scrollHeight() - viewport)),
    set: (px: number) => {
      scrollTop = px;
    },
  });
  notebook.scrollTop = 10_000;
  const end = notebook.scrollTop;
  const handle = screen.getByRole("separator", { name: "Resize results in Cell b" });

  fireEvent.pointerDown(handle, { button: 0, clientY: 500 });
  fireEvent.pointerMove(handle, { clientY: 400 });
  expect(resultsHeight("b")).toBe(COMPACT - 100);
  fireEvent.pointerUp(handle, { clientY: 400 });
  expect(notebook.scrollTop).toBe(end);

  notebook.scrollTop = end - 300;
  fireEvent.scroll(notebook);
  expect(notebook.scrollHeight).toBe(1500 + COMPACT - 100);
});

it("keeps the slack a released drag left behind when the next drag on the last cell starts", async () => {
  withRowCounts({ a: [1000], b: [1000] });
  await renderNotebook();
  const notebook = screen.getByTestId("notebook");
  const scrollHeight = () => 1500 + resultsHeight("b") + (parseFloat(notebook.style.getPropertyValue("--notebook-drag-slack")) || 0);
  let scrollTop = 0;
  Object.defineProperty(notebook, "scrollHeight", { get: scrollHeight });
  Object.defineProperty(notebook, "scrollTop", {
    get: () => (scrollTop = Math.min(scrollTop, scrollHeight() - viewport)),
    set: (px: number) => {
      scrollTop = px;
    },
  });
  notebook.scrollTop = 10_000;
  const end = notebook.scrollTop;
  const handle = screen.getByRole("separator", { name: "Resize results in Cell b" });
  fireEvent.pointerDown(handle, { button: 0, clientY: 500 });
  fireEvent.pointerMove(handle, { clientY: -10_000 });
  fireEvent.pointerUp(handle, { clientY: -10_000 });
  expect(notebook.scrollTop).toBe(end);

  fireEvent.pointerDown(handle, { button: 0, clientY: 300 });
  expect(notebook.scrollTop).toBe(end);
  fireEvent.pointerUp(handle, { clientY: 300 });
  expect(notebook.scrollTop).toBe(end);
});

it("scrolls only as far as keeps a handle in view while the arrow keys step it", async () => {
  withRowCounts({ a: [1000] });
  await renderNotebook();
  const scroll = vi.spyOn(Element.prototype, "scrollIntoView").mockImplementation(() => {});
  const handle = screen.getByRole("separator", { name: "Resize query in Cell a" });

  fireEvent.keyDown(handle, { key: "ArrowDown" });
  expect(editorHeight("a")).toBe(EDITOR + 8);
  expect(scroll.mock.contexts).toEqual([handle]);
  expect(scroll.mock.calls).toEqual([[{ block: "nearest" }]]);
});

it("follows a focused cell's results while they grow, and stops once they reach the viewport", async () => {
  withRowCounts({ a: [2] });
  await renderNotebook();
  const scroll = vi.spyOn(Element.prototype, "scrollIntoView").mockImplementation(() => {});

  act(() => withRowCounts({ a: [1000] }));
  expect(resultsHeight("a")).toBe(FILLED);
  expect(scroll).toHaveBeenCalledTimes(1);

  act(() => withRowCounts({ a: [2000] }));
  act(() => withRowCounts({ a: [3000] }));
  expect(scroll).toHaveBeenCalledTimes(1);
});

it("marks a cell stale once a cell its run inlined changes, and says which one", async () => {
  useWorkspace.getState().setRunSources("a", [{ name: "Cell b", text: FIVE_LINES }]);
  await renderNotebook();
  expect(screen.queryByTestId("stale-a")).toBeNull();
  expect(cell("a").classList.contains("notebook-cell-stale")).toBe(false);

  act(() => seedEditor("b", "SELECT 1"));
  expect(screen.getByTestId("stale-a").textContent).toBe("Stale");
  expect(screen.getByTestId("stale-a").title).toBe("Cell b changed since this cell ran. Run this cell to update it.");
  expect(cell("a").classList.contains("notebook-cell-stale")).toBe(true);

  act(() => useWorkspace.getState().renameTab("b", "orders"));
  expect(screen.getByTestId("stale-a").title).toBe("No cell is named Cell b now. Run this cell to update it.");

  act(() => useWorkspace.getState().setRunSources("a", []));
  expect(screen.queryByTestId("stale-a")).toBeNull();
});

describe("a collapsed cell", () => {
  const toggle = (id: TabId) => within(cell(id)).getByRole("button", { name: /^(Collapse|Expand) cell$/ });

  it("shows only its header with its first SQL line and row count, and opens again from the chevron", async () => {
    seedEditor("b", "\n  \n  SELECT id, name FROM customers\nWHERE id > 1");
    withRowCounts({ b: [60] });
    await renderNotebook();
    expect(within(cell("b")).queryByTestId(`cell-summary-b`)).toBeNull();

    fireEvent.click(toggle("b"));
    expect([toggle("b").getAttribute("aria-label"), toggle("b").getAttribute("aria-expanded")]).toEqual(["Expand cell", "false"]);
    expect(cell("b").querySelector(".notebook-editor, .notebook-results")).toBeNull();
    expect(screen.queryAllByRole("separator").filter((s) => s.getAttribute("aria-label")?.endsWith("in Cell b"))).toEqual([]);
    expect(within(cell("b")).getByTestId("tab-b").textContent).toBe("Cell bSELECT id, name FROM customers60 rows · 5 ms");
    expect(cell("a").querySelector(".notebook-editor")).not.toBeNull();

    fireEvent.click(toggle("b"));
    expect([toggle("b").getAttribute("aria-label"), toggle("b").getAttribute("aria-expanded")]).toEqual(["Collapse cell", "true"]);
    expect(cell("b").querySelector(".notebook-editor")).not.toBeNull();
    expect(resultsHeight("b")).toBeGreaterThan(0);
  });

  it("shows a failed run's error in its header instead of the row count", async () => {
    withRowCounts({ b: [60] });
    const run = useLive.getState().consoles.b!.run;
    if (run.kind !== "Finished") throw new Error("expected a finished run");
    const error = { severity: "ERROR", sqlstate: "42P01", message: 'relation "nope" does not exist', detail: null, hint: null, context: null, position: 14 };
    const failed = {
      ...run,
      outcome: "Failed" as const,
      statements: [...run.statements, { index: 1, range: { from: 0, to: 1 }, status: { kind: "Failed" as const, error }, outputs: [] }],
    };
    useLive.setState({ consoles: { b: { ...useLive.getState().consoles.b!, run: failed } } });
    await renderNotebook();
    fireEvent.click(toggle("b"));

    const status = within(cell("b")).getByTestId("cell-run-status-b");
    expect(status.textContent).toBe('relation "nope" does not exist');
    expect(status.className).toContain("text-danger");
  });

  it("stays collapsed when its header activates it", async () => {
    act(() => useWorkspace.getState().setCollapsed(["b"], true));
    await renderNotebook();
    fireEvent.click(within(cell("b")).getByTestId("tab-b"));
    await afterClick();
    expect([activeId(useWorkspace.getState()), toggle("b").getAttribute("aria-expanded"), cell("b").querySelector(".notebook-editor")]).toEqual([
      "b",
      "false",
      null,
    ]);
  });
});

describe("arrow keys at a cell's edge", () => {
  const content = (id: TabId) => cell(id).querySelector<HTMLElement>(".cm-content")!;
  const cursor = (id: TabId) => cursorOf(editorStateOf(id));
  const lineStart = (id: TabId, line: number) => editorStateOf(id).doc.line(line).from;

  beforeEach(() => {
    installEditorBehavior(cellNavigation);
    for (const id of IDS) seedEditor(id, FIVE_LINES);
  });

  afterEach(() => installEditorBehavior([]));

  it("moves down from the last line into the next cell's first line, and back up, at the same column", async () => {
    await renderNotebook();
    act(() => moveCursor("a", lineStart("a", 5) + 4));

    fireEvent.keyDown(content("a"), { key: "ArrowDown" });
    expect(activeId(useWorkspace.getState())).toBe("b");
    expect(cursor("b")).toEqual({ line: 1, col: 5 });
    expect(document.activeElement).toBe(content("b"));

    act(() => moveCursor("b", 2));
    fireEvent.keyDown(content("b"), { key: "ArrowUp" });
    expect(activeId(useWorkspace.getState())).toBe("a");
    expect(cursor("a")).toEqual({ line: 5, col: 3 });
    expect(document.activeElement).toBe(content("a"));
  });

  it("expands a collapsed cell it moves into", async () => {
    act(() => useWorkspace.getState().setCollapsed(["b"], true));
    await renderNotebook();
    act(() => moveCursor("a", lineStart("a", 5) + 4));
    fireEvent.keyDown(content("a"), { key: "ArrowDown" });
    expect([activeId(useWorkspace.getState()), cursor("b"), document.activeElement]).toEqual(["b", { line: 1, col: 5 }, content("b")]);
    expect(within(cell("b")).getByRole("button", { name: "Collapse cell" }).getAttribute("aria-expanded")).toBe("true");
  });

  it("keeps the column within a shorter line", async () => {
    await renderNotebook();
    act(() => moveCursor("a", lineStart("a", 5) + 11));
    fireEvent.keyDown(content("a"), { key: "ArrowDown" });
    expect(cursor("b")).toEqual({ line: 1, col: 7 });
  });

  it("stays in the cell away from its edge, past the notebook's ends, with a selection, or with a modifier", async () => {
    await renderNotebook();
    act(() => moveCursor("a", lineStart("a", 4)));
    fireEvent.keyDown(content("a"), { key: "ArrowDown" });
    act(() => moveCursor("a", 0));
    fireEvent.keyDown(content("a"), { key: "ArrowUp" });
    act(() => moveCursor("a", lineStart("a", 5)));
    fireEvent.keyDown(content("a"), { key: "ArrowDown", shiftKey: true });
    fireEvent.keyDown(content("a"), { key: "ArrowDown", altKey: true });
    fireEvent.keyDown(content("a"), { key: "ArrowDown", metaKey: true });
    const view = EditorView.findFromDOM(content("a"))!;
    act(() => view.dispatch({ selection: { anchor: lineStart("a", 5), head: lineStart("a", 5) + 2 } }));
    fireEvent.keyDown(content("a"), { key: "ArrowDown" });
    expect(activeId(useWorkspace.getState())).toBe("a");

    await act(async () => useWorkspace.getState().setActive("b"));
    act(() => moveCursor("b", lineStart("b", 5)));
    fireEvent.keyDown(content("b"), { key: "ArrowDown" });
    expect(activeId(useWorkspace.getState())).toBe("b");
  });

  it("leaves the arrows to an open completion list", async () => {
    mockIPC((cmd, args) => {
      if (cmd !== "completion_context") throw new Error(`Unexpected IPC command: ${cmd}`);
      const { text } = args as { text: string };
      return {
        kind: "Complete",
        from: text.length - 1,
        expect: { kind: "Keyword" },
        qualifier: null,
        relations: [],
        ctes: [],
        keywords: ["SELECT"],
        functions: [],
      };
    });
    await renderNotebook();
    const view = EditorView.findFromDOM(content("a"))!;
    const end = view.state.doc.length;
    act(() => view.dispatch({ changes: { from: end, insert: " s" }, selection: { anchor: end + 2 }, userEvent: "input.type" }));
    await waitFor(() => expect(completionStatus(view.state)).toBe("active"));
    // The list ignores keys for its first 75 ms, so a press that early would not reach it.
    await act(() => new Promise((resolve) => setTimeout(resolve, 100)));

    fireEvent.keyDown(content("a"), { key: "ArrowDown" });
    expect(activeId(useWorkspace.getState())).toBe("a");
    expect(completionStatus(view.state)).toBe("active");
  });
});

describe("moving a cell", () => {
  const order = () => screen.getAllByTestId(/^notebook-cell-/).map((el) => el.dataset.testid?.slice("notebook-cell-".length));
  const number = (id: TabId) => screen.getByTestId(`cell-grip-${id}`).textContent;
  const dropLine = () => document.querySelector<HTMLElement>('[data-testid="cell-drop-line"]');
  const RECTS: Record<string, DOMRect> = {
    notebook: DOMRect.fromRect({ y: 0, width: 800, height: 858 }),
    "notebook-cell-a": DOMRect.fromRect({ x: 24, y: 100, width: 752, height: 200 }),
    "notebook-cell-b": DOMRect.fromRect({ x: 24, y: 328, width: 752, height: 200 }),
  };

  beforeEach(() => {
    vi.spyOn(Element.prototype, "getBoundingClientRect").mockImplementation(function (this: Element) {
      return RECTS[this.getAttribute("data-testid") ?? ""] ?? DOMRect.fromRect();
    });
  });

  it("moves the cell with ⌥↓ and ⌥↑ on its focused title, keeps the title focused, and leaves plain arrows to move focus", async () => {
    await renderNotebook();
    const [a, b] = [screen.getByTestId("tab-a"), screen.getByTestId("tab-b")];
    act(() => a.focus());

    fireEvent.keyDown(a, { key: "ArrowDown", altKey: true });
    expect([order(), number("a"), number("b"), document.activeElement]).toEqual([["b", "a"], "02", "01", a]);
    fireEvent.keyDown(a, { key: "ArrowDown", altKey: true });
    expect([order(), document.activeElement]).toEqual([["b", "a"], a]);

    fireEvent.keyDown(a, { key: "ArrowUp" });
    expect([order(), document.activeElement]).toEqual([["b", "a"], b]);
    fireEvent.keyDown(b, { key: "ArrowDown", altKey: true });
    expect([order(), activeId(useWorkspace.getState()), document.activeElement]).toEqual([["a", "b"], "b", b]);
  });

  it("drags a cell by its number past the middle of another and drops it there, with a line where it will land", async () => {
    await renderNotebook();
    const grip = screen.getByTestId("cell-grip-a");

    fireEvent.pointerDown(grip, { button: 0, clientY: 120 });
    fireEvent.pointerMove(grip, { clientY: 122 });
    expect(cell("a").hasAttribute("data-dragging")).toBe(false);
    fireEvent.pointerMove(grip, { clientY: 400 });
    expect([cell("a").hasAttribute("data-dragging"), dropLine()]).toEqual([true, null]);
    fireEvent.pointerMove(grip, { clientY: 450 });
    expect(dropLine()?.style.top).toBe("542px");

    fireEvent.pointerUp(grip);
    expect([order(), number("a"), activeId(useWorkspace.getState()), dropLine(), cell("a").hasAttribute("data-dragging")]).toEqual([
      ["b", "a"],
      "02",
      "a",
      null,
      false,
    ]);
  });

  it("hands typing focus to the cell a drag moves: its editor, or its title while it is collapsed", async () => {
    const dragAbove = (id: TabId) => {
      const grip = screen.getByTestId(`cell-grip-${id}`);
      fireEvent.pointerDown(grip, { button: 0, clientY: 340 });
      fireEvent.pointerMove(grip, { clientY: 150 });
      fireEvent.pointerUp(grip);
    };
    await renderNotebook();
    act(() => cell("a").querySelector<HTMLElement>(".cm-content")!.focus());
    dragAbove("b");
    expect([order(), activeId(useWorkspace.getState()), document.activeElement]).toEqual([["b", "a"], "b", cell("b").querySelector(".cm-content")]);

    act(() => useWorkspace.getState().moveCell("a", 0));
    act(() => useWorkspace.getState().setCollapsed(["b"], true));
    act(() => cell("a").querySelector<HTMLElement>(".cm-content")!.focus());
    dragAbove("b");
    expect([order(), document.activeElement]).toEqual([["b", "a"], screen.getByTestId("tab-b")]);
  });

  it("puts the cell back on Esc, and changes nothing for a drop where the cell already is", async () => {
    await renderNotebook();
    const grip = screen.getByTestId("cell-grip-b");
    const { notebooks } = useWorkspace.getState();

    fireEvent.pointerDown(grip, { button: 0, clientY: 340 });
    fireEvent.pointerMove(grip, { clientY: 150 });
    expect(dropLine()?.style.top).toBe("86px");
    fireEvent.keyDown(window, { key: "Escape" });
    expect([dropLine(), cell("b").hasAttribute("data-dragging")]).toEqual([null, false]);
    fireEvent.pointerMove(grip, { clientY: 160 });
    fireEvent.pointerUp(grip);

    fireEvent.pointerDown(grip, { button: 0, clientY: 340 });
    fireEvent.pointerMove(grip, { clientY: 400 });
    fireEvent.pointerUp(grip);
    expect([order(), useWorkspace.getState().notebooks]).toEqual([["a", "b"], notebooks]);
    expect(useWorkspace.getState().notebooks).toBe(notebooks);
  });
});
