import { mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ConsoleState, DatabaseRef, NotebookSaved, NotebookText } from "../../ipc/bindings";
import { useLive } from "../../state/live";
import { activeId, allCells, shownNotebook, useWorkspace } from "../../state/workspace";
import { bindToFile } from "../../testing/notebooks";
import { dropEditor, insertAtCursor } from "../editor/editorStates";
import { openSqlTab } from "../editor/runActions";
import { deleteNotebook, startNotebook } from "./notebookActions";

const ws = () => useWorkspace.getState();
const shop: DatabaseRef = { connectionId: "c1", database: "shop" };
const asked = () => {
  const o = ws().overlay;
  return o.kind === "ask" ? { title: o.title, body: o.body } : null;
};
const titles = () => Object.values(ws().notebooks).map((n) => n.title);

let closes: [string, boolean][];
let saves: NotebookText[];

beforeEach(() => {
  vi.stubGlobal("window", { crypto });
  closes = [];
  saves = [];
  mockIPC((cmd, args) => {
    if (cmd === "notebook_save") {
      saves.push((args as { notebook: NotebookText }).notebook);
      return { binding: { path: "/q/a.sql", stamp: "s2", format: { bom: false, crlf: false } }, name: "a.sql", plain: false } satisfies NotebookSaved;
    }
    if (cmd === "console_close") {
      const { tabId, force } = args as { tabId: string; force: boolean };
      closes.push([tabId, force]);
    }
    return null;
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
  for (const id of Object.keys(ws().tabs)) dropEditor(id);
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
});

function withConsole(id: string, txn: ConsoleState["txn"]): void {
  const console: ConsoleState = {
    tabId: id,
    rev: 1,
    config: { connectionId: "c1", database: "shop", schema: null, mode: "Manual" },
    session: { kind: "Open", backendPid: 7 },
    txn,
    txnDepth: null,
    work: null,
    export: null,
    import: null,
    run: { kind: "Idle" },
  };
  useLive.setState((s) => ({ consoles: { ...s.consoles, [id]: console } }));
}

/** A second notebook on shop with two cells, the first holding a query, shown on top of the first notebook. */
function secondNotebook(): { first: string; cells: [string, string] } {
  const first = openSqlTab({ db: shop, text: "select 1" });
  const a = openSqlTab({ db: shop, text: "select 2", newNotebook: true });
  const b = openSqlTab({ db: shop });
  return { first, cells: [a, b] };
}

it("asks before deleting a notebook, rolls back its transactions without asking again, and shows the database's next notebook", async () => {
  const { first, cells } = secondNotebook();
  withConsole(cells[0], "Open");
  const deleting = deleteNotebook(shownNotebook(ws())!.id);
  await vi.waitFor(() => expect(asked()).toEqual({ title: "Delete shop 2?", body: "Its 2 cells close. Any open transactions are rolled back." }));
  ws().answer("btn");
  await deleting;
  expect([titles(), allCells(ws()), activeId(ws()), closes]).toEqual([["shop"], [first], first, [[cells[0], true]]]);
});

it("leaves out the rollback line without an open transaction, and keeps the notebook on Cancel", async () => {
  const { cells } = secondNotebook();
  withConsole(cells[0], "Idle");
  const id = shownNotebook(ws())!.id;
  const deleting = deleteNotebook(id);
  await vi.waitFor(() => expect(asked()).toEqual({ title: "Delete shop 2?", body: "Its 2 cells close." }));
  ws().close();
  await deleting;
  expect([titles(), shownNotebook(ws())?.cells, closes]).toEqual([["shop", "shop 2"], cells, []]);
});

it("deletes a blank notebook without asking", async () => {
  openSqlTab({ db: shop, text: "select 1" });
  startNotebook();
  await deleteNotebook(shownNotebook(ws())!.id);
  expect([titles(), asked()]).toEqual([["shop"], null]);
});

/** The second notebook, bound to a file, with its first cell edited since. */
function unsavedFileNotebook(): { id: string; cells: [string, string] } {
  const { cells } = secondNotebook();
  const id = shownNotebook(ws())!.id;
  bindToFile(id);
  insertAtCursor(cells[0], "-- edited ");
  return { id, cells };
}

it("asks once to save an unsaved file notebook, and a Cancel keeps every cell", async () => {
  const { id, cells } = unsavedFileNotebook();
  const deleting = deleteNotebook(id);
  await vi.waitFor(() => expect(asked()).toEqual({ title: "Save changes to shop 2?", body: "Your changes are lost if you don't save them." }));
  ws().close();
  await deleting;
  expect([titles(), shownNotebook(ws())?.cells, saves]).toEqual([["shop", "shop 2"], cells, []]);
});

it("deletes an unsaved file notebook on Don't Save without asking again, and on Save writes every cell first", async () => {
  const discarded = unsavedFileNotebook();
  const deleting = deleteNotebook(discarded.id);
  await vi.waitFor(() => expect(asked()?.title).toBe("Save changes to shop 2?"));
  ws().answer("discard");
  await deleting;
  expect([titles(), saves, asked()]).toEqual([["shop"], [], null]);

  const kept = unsavedFileNotebook();
  const saving = deleteNotebook(kept.id);
  await vi.waitFor(() => expect(asked()?.title).toBe("Save changes to shop 2?"));
  ws().answer("save");
  await saving;
  expect([titles(), saves.map((n) => n.cells.map((c) => c.text))]).toEqual([["shop"], [["-- edited select 2", ""]]]);
});

it("deletes a saved file notebook without asking, since its file holds every cell", async () => {
  secondNotebook();
  bindToFile(shownNotebook(ws())!.id);
  await deleteNotebook(shownNotebook(ws())!.id);
  expect([titles(), asked()]).toEqual([["shop"], null]);
});
