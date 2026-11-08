import { mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ConsoleState, DisconnectError, FileWriteError, NotebookSaved, NotebookText } from "../../ipc/bindings";
import { useLive } from "../../state/live";
import { setCell, useTableEdits } from "../../state/tableEdits";
import { allCells, shownNotebook, useWorkspace } from "../../state/workspace";
import { bindToFile } from "../../testing/notebooks";
import { notebookDirty } from "../notebooks/notebookDirty";
import { closeTab } from "./closeTab";
import { dropEditor, insertAtCursor, seedEditor } from "./editorStates";
import { openSqlTab } from "./runActions";

const ws = () => useWorkspace.getState();
const flush = () => new Promise((r) => setTimeout(r, 0));
const question = () => {
  const o = ws().overlay;
  return o.kind === "ask" ? o.title : null;
};

let closes: boolean[];
let openTransaction: boolean;
let saves: string[];
let saveFails: boolean;
let started: string[];
let saveGate: Promise<void>;

beforeEach(() => {
  vi.stubGlobal("window", { crypto });
  closes = [];
  openTransaction = false;
  saves = [];
  saveFails = false;
  started = [];
  saveGate = Promise.resolve();
  mockIPC((cmd, args) => {
    if (cmd === "notebook_save") {
      const text = (args as { notebook: NotebookText }).notebook.cells.map((c) => c.text).join("|");
      started.push(text);
      return saveGate.then(() => {
        if (saveFails) throw { kind: "PermissionDenied" } satisfies FileWriteError;
        saves.push(text);
        return { binding: { path: "/q/a.sql", stamp: "s2", format: { bom: false, crlf: false } }, name: "a.sql", plain: false } satisfies NotebookSaved;
      });
    }
    if (cmd !== "console_close") return null;
    const force = (args as { force: boolean }).force;
    closes.push(force);
    if (openTransaction && !force) throw { kind: "OpenTransactions", tabs: [] } satisfies DisconnectError;
    return null;
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
  for (const id of Object.keys(ws().tabs)) dropEditor(id);
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
  useTableEdits.setState(useTableEdits.getInitialState(), true);
});

function tabWithConsole(): string {
  const id = openSqlTab({ db: { connectionId: "c1", database: "shop" } });
  const console: ConsoleState = {
    tabId: id,
    rev: 1,
    config: { connectionId: "c1", database: "shop", schema: null, mode: "Manual" },
    session: { kind: "Open", backendPid: 7 },
    txn: "Open",
    txnDepth: null,
    work: null,
    export: null,
    import: null,
    run: { kind: "Idle" },
  };
  useLive.setState({ consoles: { [id]: console } });
  return id;
}

const stageEdit = (id: string) => useTableEdits.getState().edit(id, (e) => setCell(e, [{ column: "id", value: "1" }], "total", "9", "1"));

it("closes a tab with nothing to lose without asking", async () => {
  const id = openSqlTab();
  await closeTab(id);
  expect([allCells(ws()), question(), closes]).toEqual([[], null, []]);
});

it("asks to discard staged edits, then to roll back, and closes only after both", async () => {
  const id = tabWithConsole();
  openTransaction = true;
  stageEdit(id);
  const closing = closeTab(id);
  await vi.waitFor(() => expect(question()).toBe("Close tab with unsaved changes?"));
  ws().answer("btn");
  await vi.waitFor(() => expect(question()).toBe("Close tab with an open transaction?"));
  expect(allCells(ws())).toEqual([id]);
  ws().answer("btn");
  await closing;
  expect([allCells(ws()), closes]).toEqual([[], [false, true]]);
});

it("keeps the tab and its transaction when the rollback question is cancelled", async () => {
  const id = tabWithConsole();
  openTransaction = true;
  const closing = closeTab(id);
  await vi.waitFor(() => expect(question()).toBe("Close tab with an open transaction?"));
  ws().close();
  await closing;
  await flush();
  expect([allCells(ws()), closes]).toEqual([[id], [false]]);
});

it("keeps the tab and its staged edits when the discard question is cancelled", async () => {
  const id = tabWithConsole();
  stageEdit(id);
  const closing = closeTab(id);
  await vi.waitFor(() => expect(question()).toBe("Close tab with unsaved changes?"));
  ws().close();
  await closing;
  expect([allCells(ws()), closes, Object.keys(useTableEdits.getState().byTab)]).toEqual([[id], [], [id]]);
});

/** The only cell of a notebook bound to a file, edited since the file was read. */
function dirtyFileTab(): string {
  const id = tabWithConsole();
  seedEditor(id, "select 1;");
  bindToFile(shownNotebook(ws())!.id);
  insertAtCursor(id, "-- edited ");
  return id;
}

it("asks to save a file notebook whose last cell closes, and Don't Save closes it without writing", async () => {
  const id = dirtyFileTab();
  const closing = closeTab(id);
  await vi.waitFor(() => expect(question()).toBe("Save changes to shop?"));
  expect(ws().overlay).toMatchObject({
    testId: "ask",
    choices: [
      { id: "discard", label: "Don't Save" },
      { id: "save", label: "Save" },
    ],
  });
  ws().answer("discard");
  await closing;
  expect([allCells(ws()), saves]).toEqual([[], []]);
});

it("keeps an unsaved file notebook's last cell open when the save question is cancelled", async () => {
  const id = dirtyFileTab();
  const closing = closeTab(id);
  await vi.waitFor(() => expect(question()).toBe("Save changes to shop?"));
  ws().close();
  await closing;
  expect([allCells(ws()), saves, closes]).toEqual([[id], [], []]);
});

it("saves, then asks to roll back, then closes", async () => {
  const id = dirtyFileTab();
  openTransaction = true;
  const closing = closeTab(id);
  await vi.waitFor(() => expect(question()).toBe("Save changes to shop?"));
  ws().answer("save");
  await vi.waitFor(() => expect(question()).toBe("Close tab with an open transaction?"));
  expect(saves).toEqual(["-- edited select 1;"]);
  ws().answer("btn");
  await closing;
  expect([allCells(ws()), closes]).toEqual([[], [false, true]]);
});

it("asks again when the text changes while the close-time save is under way", async () => {
  const id = dirtyFileTab();
  let release = () => {};
  saveGate = new Promise((r) => (release = r));
  const closing = closeTab(id);
  await vi.waitFor(() => expect(question()).toBe("Save changes to shop?"));
  ws().answer("save");
  await vi.waitFor(() => expect(started).toHaveLength(1));
  insertAtCursor(id, "-- later ");
  release();
  await vi.waitFor(() => expect(question()).toBe("Save changes to shop?"));
  expect([allCells(ws()), saves]).toEqual([[id], ["-- edited select 1;"]]);
  ws().answer("save");
  await closing;
  expect([allCells(ws()), saves]).toEqual([[], ["-- edited select 1;", "-- edited -- later select 1;"]]);
});

it("keeps the tab open when its save fails", async () => {
  const id = dirtyFileTab();
  saveFails = true;
  const closing = closeTab(id);
  await vi.waitFor(() => expect(question()).toBe("Save changes to shop?"));
  ws().answer("save");
  await closing;
  expect([allCells(ws()), closes, ws().toast?.text]).toEqual([[id], [], "You don't have permission to save /q/a.sql. Use Save As to save a copy elsewhere."]);
});

it("closes a file notebook's last cell without asking when the file holds it", async () => {
  const id = openSqlTab({ text: "select 1;" });
  bindToFile(shownNotebook(ws())!.id);
  await closeTab(id);
  expect([allCells(ws()), question()]).toEqual([[], null]);
});

it("closes one of a file notebook's cells without asking, which leaves the notebook unsaved", async () => {
  const a = openSqlTab({ text: "select 1;" });
  const b = openSqlTab({ text: "select 2;" });
  const notebook = shownNotebook(ws())!.id;
  bindToFile(notebook);
  await closeTab(b);
  expect([allCells(ws()), question(), notebookDirty(notebook)]).toEqual([[a], null, true]);
});
