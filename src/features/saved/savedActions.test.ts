import { mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, expect, it, vi } from "vitest";
import type { SavedQueries, SavedQuery, SaveQueryReply } from "../../ipc/bindings";
import { connectedState, connectionConfig } from "../../testing/connections";
import { useLive } from "../../state/live";
import { activeId, useWorkspace } from "../../state/workspace";
import { dropEditor, moveCursor, textOf } from "../editor/editorStates";
import { openSqlTab } from "../editor/runActions";
import { copySaved, insertSaved, openSaved, saveActiveQuery, saveHistoryQuery } from "./savedActions";

const query = (id: number, name: string, sql: string, databaseKind: SavedQuery["databaseKind"] = "Postgres"): SavedQuery => ({
  id,
  name,
  sql,
  databaseKind,
  excerpt: sql,
  createdAtMs: 1,
  usedAtMs: 1,
});
const list = (rev: number, ...items: SavedQuery[]): SavedQueries => ({ rev, items });

afterEach(() => {
  for (const id of Object.keys(useWorkspace.getState().tabs)) dropEditor(id);
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
  vi.unstubAllGlobals();
});

function recordIpc(reply: (cmd: string) => unknown = () => null) {
  const calls: { cmd: string; args: unknown }[] = [];
  vi.stubGlobal("window", { crypto });
  mockIPC((cmd, args) => {
    calls.push({ cmd, args });
    return reply(cmd);
  });
  return calls;
}

it("saves the statement at the cursor of the active tab and opens the palette on the new entry's name", async () => {
  const saved: SaveQueryReply = { id: 4, alreadySaved: false, queries: list(2, query(4, "select 2", "select 2")) };
  const calls = recordIpc(() => saved);
  const tab = openSqlTab({ text: "select 1;\nselect 2;" });
  moveCursor(tab, 12);
  await saveActiveQuery();
  expect(calls).toEqual([
    { cmd: "saved_query_save", args: { text: "select 1;\nselect 2;", target: { kind: "cursor", offset: 12 }, databaseKind: "Postgres" } },
  ]);
  expect(useWorkspace.getState().overlay).toEqual({ kind: "saved", entry: { kind: "name", id: 4 } });
  expect(useLive.getState().savedQueries).toEqual(saved.queries);
});

it("opens the palette on the entry already saved instead of naming a duplicate", async () => {
  recordIpc(() => ({ id: 4, alreadySaved: true, queries: list(2, query(4, "select 2", "select 2")) }));
  openSqlTab({ text: "select 2" });
  await saveActiveQuery();
  expect(useWorkspace.getState().overlay).toEqual({ kind: "saved", entry: { kind: "existing", id: 4 } });
});

it("says why nothing was saved, and saves nothing with no tab or while a dialog is up", async () => {
  const calls = recordIpc(() => {
    throw { kind: "NothingToSave" };
  });
  await saveActiveQuery();
  expect(useWorkspace.getState().toast?.text).toBe("Open a query tab to save a query.");
  openSqlTab({ text: "select 1" });
  useWorkspace.getState().open({ kind: "history" });
  await saveActiveQuery();
  expect(calls).toEqual([]);
  useWorkspace.getState().close();
  await saveActiveQuery();
  expect(calls).toHaveLength(1);
  expect(useWorkspace.getState().toast?.text).toBe("There is no SQL to save. Select some, or put the cursor in a statement.");
  expect(useWorkspace.getState().overlay).toEqual({ kind: "none" });
});

it("saves a history query whole in its dialect, closing history for the palette", async () => {
  const calls = recordIpc(() => ({ id: 9, alreadySaved: false, queries: list(2, query(9, "Locks", "select * from sys.dm_tran_locks", "SqlServer")) }));
  useWorkspace.getState().open({ kind: "history" });
  const saving = saveHistoryQuery("select * from sys.dm_tran_locks", "SqlServer");
  expect(useWorkspace.getState().overlay).toEqual({ kind: "none" });
  await saving;
  expect(calls[0]).toEqual({ cmd: "saved_query_save", args: { text: "select * from sys.dm_tran_locks", target: { kind: "all" }, databaseKind: "SqlServer" } });
  expect(useWorkspace.getState().overlay).toEqual({ kind: "saved", entry: { kind: "name", id: 9 } });
});

it("writes the clipboard before any IPC, then records the use and names the copy", async () => {
  const order: string[] = [];
  recordIpc((cmd) => {
    order.push(cmd);
    return list(3, query(1, "Active sessions", "select pid from pg_stat_activity"));
  });
  vi.stubGlobal("navigator", { userAgent: "", clipboard: { writeText: (text: string) => (order.push(`clipboard:${text}`), Promise.resolve()) } });
  const copying = copySaved(query(1, "Active sessions", "select pid from pg_stat_activity"));
  expect(order).toEqual(["clipboard:select pid from pg_stat_activity"]);
  expect(await copying).toBe(true);
  await vi.waitFor(() => expect(useLive.getState().savedQueries.rev).toBe(3));
  expect(order).toEqual(["clipboard:select pid from pg_stat_activity", "saved_query_used"]);
  expect(useWorkspace.getState().toast?.text).toBe("Copied “Active sessions”.");
});

it("records nothing when the clipboard refuses the write", async () => {
  const calls = recordIpc();
  vi.stubGlobal("navigator", { userAgent: "", clipboard: { writeText: () => Promise.reject(new Error("denied")) } });
  expect(await copySaved(query(1, "Active sessions", "select 1"))).toBe(false);
  expect([calls, useWorkspace.getState().toast]).toEqual([[], null]);
});

it("inserts at the active tab's cursor and records the use", async () => {
  const calls = recordIpc(() => list(2));
  const tab = openSqlTab({ text: "select 1;" });
  insertSaved(query(5, "Locks", "select * from pg_locks;\n"));
  expect(textOf(tab)).toBe("select * from pg_locks;\nselect 1;");
  await vi.waitFor(() => expect(calls).toEqual([{ cmd: "saved_query_used", args: { id: 5 } }]));
});

it("opens a query in a new tab on the active tab's database when the dialects match, else on a connected database of its dialect", () => {
  recordIpc(() => list(2));
  const pg = connectionConfig("c-pg", "pg");
  const ms = { ...connectionConfig("c-ms", "ms"), databaseKind: "SqlServer" as const, database: "master" };
  const idle = { ...connectionConfig("c-ms-idle", "ms idle"), databaseKind: "SqlServer" as const };
  useWorkspace.setState({ configs: { [idle.id]: idle, [pg.id]: pg, [ms.id]: ms } });
  useLive.setState({
    connections: {
      [ms.id]: {
        ...connectedState(ms.id, []),
        status: {
          kind: "Connected",
          server: { version: "16", versionNum: 160000, encoding: "UTF8" },
          instance: { configuredDatabase: "master", home: "master", showSystemDatabases: false, databases: [] },
        },
      },
    },
  });
  openSqlTab({ db: { connectionId: pg.id, database: "reporting" }, text: "" });

  openSaved(query(1, "Revenue", "select 42"));
  const opened = useWorkspace.getState().tabs[activeId(useWorkspace.getState())!]!;
  expect([opened.title, opened.console.db, textOf(opened.id)]).toEqual(["Revenue", { connectionId: pg.id, database: "reporting" }, "select 42"]);

  openSaved(query(2, "Locks", "select * from sys.dm_tran_locks", "SqlServer"));
  expect(useWorkspace.getState().tabs[activeId(useWorkspace.getState())!]!.console.db).toEqual({ connectionId: ms.id, database: "master" });
});

it("never opens a query on a database of the other dialect", () => {
  recordIpc(() => list(2));
  const pg = connectionConfig("c-pg", "pg");
  const idle = { ...connectionConfig("c-ms-idle", "ms idle"), databaseKind: "SqlServer" as const, database: "master" };
  useWorkspace.setState({ configs: { [pg.id]: pg } });
  openSqlTab({ db: { connectionId: pg.id, database: "shop" }, text: "" });
  const active = () => useWorkspace.getState().tabs[activeId(useWorkspace.getState())!]!;

  openSaved(query(1, "Locks", "select * from sys.dm_tran_locks", "SqlServer"));
  expect([active().title, active().console.db]).toEqual(["Locks", null]);

  useWorkspace.setState({ configs: { [pg.id]: pg, [idle.id]: idle } });
  openSqlTab({ db: { connectionId: pg.id, database: "shop" }, text: "" });
  openSaved(query(1, "Locks", "select * from sys.dm_tran_locks", "SqlServer"));
  expect(active().console.db).toEqual({ connectionId: idle.id, database: "master" });
});

it("opens a saved query on the shown database when that database has no notebook yet", () => {
  recordIpc(() => list(2));
  const pg = connectionConfig("c-pg", "pg");
  useWorkspace.setState({ configs: { [pg.id]: pg } });
  openSqlTab({ db: { connectionId: pg.id, database: "shop" }, text: "" });
  useWorkspace.getState().showDatabase({ connectionId: pg.id, database: "reporting" });

  openSaved(query(1, "Revenue", "select 42"));
  expect(useWorkspace.getState().tabs[activeId(useWorkspace.getState())!]!.console.db).toEqual({ connectionId: pg.id, database: "reporting" });
});
