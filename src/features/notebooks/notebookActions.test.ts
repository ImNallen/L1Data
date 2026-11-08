import { mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ConsoleState, DatabaseRef, FileSaved } from "../../ipc/bindings";
import { useLive } from "../../state/live";
import { activeId, allCells, shownNotebook, useWorkspace } from "../../state/workspace";
import { dropEditor, insertAtCursor, seedEditor } from "../editor/editorStates";
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

beforeEach(() => {
  vi.stubGlobal("window", { crypto });
  closes = [];
  mockIPC((cmd, args) => {
    if (cmd === "file_save") return { binding: { path: "/q/a.sql", stamp: "s2", format: { bom: false, crlf: false } }, name: "a.sql" } satisfies FileSaved;
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

it("asks to save each changed file, and a Cancel keeps that cell and the ones after it", async () => {
  const { cells } = secondNotebook();
  const [a, b] = cells;
  seedEditor(a, "select 2", "seeded");
  ws().bindFile(a, { binding: { path: "/q/a.sql", stamp: "s1", format: { bom: false, crlf: false } }, name: "a.sql" });
  insertAtCursor(a, "-- edited ");
  const deleting = deleteNotebook(shownNotebook(ws())!.id);
  await vi.waitFor(() => expect(asked()?.title).toBe("Delete shop 2?"));
  ws().answer("btn");
  await vi.waitFor(() => expect(asked()?.title).toBe("Save changes to a.sql?"));
  ws().close();
  await deleting;
  expect([titles(), shownNotebook(ws())?.cells]).toEqual([
    ["shop", "shop 2"],
    [a, b],
  ]);
});
