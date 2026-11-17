import { mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { SavedQuery, TabId } from "../../ipc/bindings";
import { useLive } from "../../state/live";
import { notebookOf } from "../../state/notebooks";
import { activeId, DEFAULT_SETTINGS, shownNotebook, useWorkspace } from "../../state/workspace";
import { connectionConfig } from "../../testing/connections";
import { fakeEngine } from "../../testing/runEngine";
import { sendMessage } from "../assistant/assistantActions";
import { closeTab } from "../editor/closeTab";
import { docOf, dropEditor, textOf } from "../editor/editorStates";
import { cancelActive, explainActive, formatActive, openMapTab, openSqlTab, runActive, runOrChoose, setTabSchema } from "../editor/runActions";
import { insertHistoryQuery } from "../history/historyActions";
import { navigationItems } from "../navigation/navigationItems";
import { insertSaved, saveActiveQuery } from "../saved/savedActions";
import { insertRoutine } from "../sidebar/ConnectionTree";
import { notebookSources, staleReason } from "./cellReferences";
import { deleteNotebook } from "./notebookActions";
import { runNotebook } from "./notebookRun";

const ws = () => useWorkspace.getState();
const db = { connectionId: "prod", database: "shop" };

let calls: { cmd: string; args: Record<string, unknown> }[];
let sql: TabId;
let map: TabId;
let after: TabId;

const sent = () => calls.map((c) => c.cmd);
const settle = () => new Promise((resolve) => setTimeout(resolve, 80));

beforeEach(() => {
  vi.stubGlobal("window", { crypto });
  calls = [];
  mockIPC((cmd, args) => {
    calls.push({ cmd, args: args as Record<string, unknown> });
    if (cmd === "console_run") return { runId: 1, statements: [], sources: [] };
    if (cmd === "script_params" || cmd === "cell_references") return [];
    if (cmd === "script_outline") return { statements: [], atCursor: null, unitKind: "Statement" };
    if (cmd === "saved_query_used") return { rev: 1, items: [] };
    return null;
  });
  useWorkspace.setState({ configs: { prod: connectionConfig("prod", "prod-postgres") } });
  sql = openSqlTab({ db, text: "select 1", title: "q" });
  map = openMapTab({ db, schema: "sales", after: sql });
  after = openSqlTab({ db, text: "select {{q}}", title: "after" });
  ws().setActive(map);
  calls = [];
});

afterEach(() => {
  expect(docOf(map), "a map cell never gets an editor buffer").toBeUndefined();
  vi.unstubAllGlobals();
  for (const id of Object.keys(ws().tabs)) dropEditor(id);
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
});

it("opens as a titled cell after the cell it follows, drawing the schema it was given", () => {
  expect(shownNotebook(ws())?.cells).toEqual([sql, map, after]);
  expect(ws().tabs[map]).toEqual({
    id: map,
    title: "Schema map",
    console: { db, mode: "AutoCommit", schema: "sales" },
    table: null,
    map: { columns: "All" },
  });
});

describe("with a map cell active", () => {
  it("runs, explains, formats, chooses, and cancels nothing", async () => {
    runActive();
    runActive({ kind: "all" });
    explainActive("Explain");
    explainActive("ExplainAnalyze");
    await formatActive();
    await runOrChoose();
    await runOrChoose(true);
    expect(cancelActive()).toBe(false);
    await settle();
    expect(sent()).toEqual([]);

    ws().setActive(sql);
    runActive({ kind: "all" });
    await vi.waitFor(() => expect(sent()).toContain("console_run"));
    expect(sent()).toContain("console_configure");
  });

  it("sets its schema without configuring a session", async () => {
    await setTabSchema(map, "public");
    expect([ws().tabs[map]?.console.schema, sent()]).toEqual(["public", []]);
  });

  it("is skipped by Run all, and Run from here runs the cells after it", async () => {
    const engine = fakeEngine();
    const notebook = notebookOf(ws().notebooks, map)!.id;
    await runNotebook(notebook, "fromActive");
    expect([engine.sent.map((s) => s.tabId), ws().toast?.text]).toEqual([[after], "Ran 1 cell."]);
    await runNotebook(notebook, "all");
    expect([engine.sent.map((s) => s.tabId), ws().toast?.text]).toEqual([[after, sql, after], "Ran 2 cells."]);
    expect(engine.checked).toEqual(["select {{q}}", "select 1", "select {{q}}"]);
  });

  it("is never a {{name}} source, never stale, and saves no query", async () => {
    expect(notebookSources(after).map((s) => s.name)).toEqual(["q", "after"]);
    expect(staleReason(map)).toBe("");
    await saveActiveQuery();
    expect([sent(), ws().toast?.text]).toEqual([[], "Open a query tab to save a query."]);
  });

  it("puts an inserted saved query, history query, or routine in a new SQL cell right after it", () => {
    const saved: SavedQuery = { id: 1, name: "Saved", sql: "select 'saved'", databaseKind: "Postgres", excerpt: "", createdAtMs: 0, usedAtMs: 0 };
    insertSaved(saved);
    const first = activeId(ws())!;
    ws().setActive(map);
    expect(insertHistoryQuery("select 'history'")).toBe(true);
    const second = activeId(ws())!;
    ws().setActive(map);
    insertRoutine(db, { oid: 1, name: "f", quoted: "f", args: "", returns: "int", kind: "Function", insertText: "f()" });
    const third = activeId(ws())!;
    expect(shownNotebook(ws())?.cells).toEqual([sql, map, third, second, first, after]);
    expect([first, second, third].map((id) => [textOf(id), ws().tabs[id]?.console])).toEqual([
      ["select 'saved'", { db, mode: "AutoCommit", schema: "sales" }],
      ["select 'history'", { db, mode: "AutoCommit", schema: "sales" }],
      ["f()", { db, mode: "AutoCommit", schema: "sales" }],
    ]);
    expect(sent()).toEqual(["saved_query_used"]);

    ws().setActive(sql);
    insertSaved(saved);
    expect([textOf(sql), shownNotebook(ws())?.cells.length]).toEqual(["select 'saved'select 1", 6]);
  });

  it("tells the assistant only which cell and database it is", async () => {
    useWorkspace.setState({ settings: { ...DEFAULT_SETTINGS, assistantAccess: "Full" } });
    await sendMessage("c4a70000-0000-4000-8000-000000000001", "what links here?", true);
    const request = calls.find((c) => c.cmd === "assistant_send")?.args.request as { tab: unknown } | undefined;
    expect(request?.tab).toEqual({ tabId: map, title: "Schema map", db, text: "", target: { kind: "all" }, attach: null });
  });

  it("is listed in quick navigation as a schema map on its database and schema", () => {
    const items = navigationItems({ ...ws(), connections: useLive.getState().connections, views: {} });
    expect(items.find((i) => i.destination.kind === "tab" && i.destination.tabId === map)).toMatchObject({
      label: "Schema map",
      kindLabel: "Schema map",
      context: "prod-postgres / shop / sales",
      action: "Focus active tab",
    });
  });

  it("follows an edit of its connection's database, as a query cell does", () => {
    ws().upsertConfig({ ...connectionConfig("prod", "prod-postgres"), database: "reporting" });
    ws().upsertConfig({ ...connectionConfig("prod", "prod-postgres"), database: "shop" });
    expect(ws().tabs[map]?.console.db).toEqual(db);
    ws().upsertConfig({ ...connectionConfig("prod", "prod-postgres"), database: "reporting" });
    expect([ws().tabs[map]?.console.db, ws().tabs[sql]?.console.db]).toEqual([
      { connectionId: "prod", database: "reporting" },
      { connectionId: "prod", database: "reporting" },
    ]);
  });
});

it("keeps a notebook that holds only a map cell from closing unasked, and closes the cell with no session to end", async () => {
  await closeTab(sql);
  await closeTab(after);
  calls = [];
  const notebook = notebookOf(ws().notebooks, map)!.id;
  void deleteNotebook(notebook);
  await vi.waitFor(() => expect(ws().overlay.kind).toBe("ask"));
  ws().close();
  await settle();
  expect(ws().tabs[map]).toBeDefined();
  await closeTab(map);
  expect([ws().tabs[map], ws().notebooks[notebook], sent()]).toEqual([undefined, undefined, []]);
});
