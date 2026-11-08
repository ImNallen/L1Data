import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { ConnectionConfig, DatabaseRef, FileSaved, Workspace } from "../ipc/bindings";
import { activeId, allCells, DEFAULT_SETTINGS, EMPTY_QUERY, shownDb, tabFromSnapshot, useWorkspace, workspaceSnapshot, type Tab } from "./workspace";

const config = (id: string): ConnectionConfig => ({
  id,
  name: id,
  host: "localhost",
  port: 5433,
  database: "shop",
  user: "l1data",
  sslMode: "Prefer",
  caCertPath: null,
  databaseKind: "Postgres",
  hasPassword: true,
  color: null,
  ssh: null,
  environment: null,
  sqlServerAuth: null,
});

describe("workspace.addTab", () => {
  it("adds a tab at the end, or right after the tab it names, and activates it", () => {
    const tab = (id: string) => ({ id, title: id, console: { db: null, mode: "AutoCommit" as const }, table: null, file: null });
    const ws = useWorkspace.getState();
    ws.addTab(tab("a"));
    ws.addTab(tab("b"));
    ws.addTab(tab("c"), "a");
    ws.addTab(tab("d"), "gone");
    expect(allCells(useWorkspace.getState())).toEqual(["a", "c", "b", "d"]);
    expect(activeId(useWorkspace.getState())).toBe("d");
  });
});

describe("workspace.upsertConfig", () => {
  it("binds tabs that have no connection and leaves bound tabs alone", () => {
    const ws = useWorkspace.getState();
    ws.addTab({ id: "free", title: "Untitled-1.sql", console: { db: null, mode: "AutoCommit" }, table: null, file: null });
    ws.addTab({
      id: "bound",
      title: "Untitled-2.sql",
      console: { db: { connectionId: "old", database: "reporting" }, mode: "Manual" },
      table: null,
      file: null,
    });
    ws.upsertConfig(config("new"));
    const tabs = useWorkspace.getState().tabs;
    expect(tabs.free?.console.db).toEqual({ connectionId: "new", database: "shop" });
    expect(tabs.bound?.console.db).toEqual({ connectionId: "old", database: "reporting" });
  });

  it("moves query tabs on the connection's configured database when that database is edited, but not table tabs", () => {
    const ws = useWorkspace.getState();
    ws.upsertConfig(config("edited"));
    const tab = (id: string, title: string, database: string, connectionId = "edited") =>
      ws.addTab({ id, title, console: { db: { connectionId, database }, mode: "AutoCommit" }, table: null, file: null });
    tab("home", "a.sql", "shop");
    tab("elsewhere", "b.sql", "reporting");
    tab("otherConn", "c.sql", "shop", "other");
    ws.addTab({
      id: "orders",
      title: "orders",
      console: { db: { connectionId: "edited", database: "shop" }, mode: "AutoCommit" },
      table: { schema: "public", table: "orders", page: 0, query: EMPTY_QUERY },
      file: null,
    });
    ws.upsertConfig({ ...config("edited"), database: "shop_v2" });
    const target = (id: string) => useWorkspace.getState().tabs[id]!.console.db;
    expect(target("home")).toEqual({ connectionId: "edited", database: "shop_v2" });
    expect(target("elsewhere")).toEqual({ connectionId: "edited", database: "reporting" });
    expect(target("otherConn")).toEqual({ connectionId: "other", database: "shop" });
    expect(target("orders")).toEqual({ connectionId: "edited", database: "shop" });
    ws.upsertConfig({ ...config("edited"), database: "shop_v2", name: "renamed" });
    expect(target("home")).toEqual({ connectionId: "edited", database: "shop_v2" });
  });
});

describe("file tabs", () => {
  afterEach(() => useWorkspace.setState(useWorkspace.getInitialState(), true));
  const saved: FileSaved = { binding: { path: "/q/report.sql", stamp: "s1", format: { bom: true, crlf: true } }, name: "report.sql" };
  const scratch = () =>
    useWorkspace.getState().addTab({ id: "t", title: "Untitled-1.sql", console: { db: null, mode: "AutoCommit" }, table: null, file: null });

  it("names a tab after the file it binds to, and refuses to rename it", () => {
    scratch();
    const ws = useWorkspace.getState();
    ws.bindFile("t", saved);
    ws.renameTab("t", "mine");
    expect(useWorkspace.getState().tabs.t).toMatchObject({ title: "report.sql", file: { binding: saved.binding, health: "ok" } });
  });

  it("saves a file tab's binding with the dirty bit read from the same buffer as its text, and restores it", () => {
    scratch();
    useWorkspace.getState().bindFile("t", saved);
    const snap = workspaceSnapshot(useWorkspace.getState(), { textOf: () => "select 1;", isDirty: (id) => id === "t" });
    expect(snap.tabs[0]).toMatchObject({ title: "report.sql", text: "select 1;", file: { binding: saved.binding, dirty: true } });
    expect(tabFromSnapshot(snap.tabs[0]!, {}).file).toEqual({ binding: saved.binding, health: "ok" });
  });
});

describe("workspace.setResultDisplay", () => {
  it("keeps each result's choice within a run and forgets the run before", () => {
    const ws = useWorkspace.getState();
    ws.setResultDisplay("t", { key: "t:1:0", display: "chart", chart: { kind: "Bar", x: 0, y: 1 } });
    ws.setResultDisplay("t", { key: "t:1:1", display: "rows", chart: null });
    expect(useWorkspace.getState().resultDisplay.t?.["t:1:0"]?.display).toBe("chart");
    expect(useWorkspace.getState().resultDisplay.t?.["t:1:1"]?.display).toBe("rows");
    ws.setResultDisplay("t", { key: "t:2:0", display: "chart", chart: null });
    expect(Object.keys(useWorkspace.getState().resultDisplay.t ?? {})).toEqual(["t:2:0"]);
  });
});

describe("notebooks", () => {
  beforeEach(() => useWorkspace.setState(useWorkspace.getInitialState(), true));
  afterEach(() => useWorkspace.setState(useWorkspace.getInitialState(), true));

  const shop = { connectionId: "c", database: "shop" };
  const postgres = { connectionId: "c", database: "postgres" };
  const cell = (id: string, db: DatabaseRef | null = shop): Tab => ({ id, title: `${id}.sql`, console: { db, mode: "AutoCommit" }, table: null, file: null });
  const ws = () => useWorkspace.getState();
  const shape = () => Object.values(ws().notebooks).map((n) => [n.title, n.cells, n.active]);

  /** A workspace as a build from before notebooks saved it, with the two fields the engine's reader adds. */
  const oldWorkspace: Workspace = {
    ...JSON.parse(`{
      "tabs": [
        { "id": "a", "title": "Untitled-1.sql", "text": "select 1", "console": { "connectionId": "c", "database": "shop", "schema": null, "mode": "AutoCommit" }, "table": null, "file": null },
        { "id": "b", "title": "Untitled-2.sql", "text": "select 2", "console": { "connectionId": "c", "database": "postgres", "schema": null, "mode": "Manual" }, "table": null, "file": null },
        { "id": "c", "title": "orders", "text": null, "console": { "connectionId": "c", "database": "shop", "schema": null, "mode": "AutoCommit" },
          "table": { "schema": "public", "table": "orders", "page": 0, "query": { "orderBy": "", "filters": [], "where": "" } }, "file": null },
        { "id": "d", "title": "scratch.sql", "text": "", "console": { "connectionId": "gone", "database": "shop", "schema": null, "mode": "AutoCommit" }, "table": null, "file": null }
      ],
      "active": "b",
      "layout": { "sidebar": true, "results": true, "rightPanel": null, "resultsHeight": 320, "sidebarWidth": 280, "rightPanelWidth": 341, "assistantChat": null }
    }`),
    notebooks: [],
    shown: null,
  };

  it("groups a workspace saved before notebooks into one notebook per database, in saved order, showing the old active cell", () => {
    ws().restore(oldWorkspace, [config("c")], DEFAULT_SETTINGS);
    expect(shape()).toEqual([
      ["shop", ["a", "c"], "a"],
      ["postgres", ["b"], "b"],
      ["No connection", ["d"], "d"],
    ]);
    expect([shownDb(ws()), activeId(ws())]).toEqual([postgres, "b"]);
  });

  it("restores the notebooks and the shown one it saved", () => {
    ws().restore(oldWorkspace, [config("c")], DEFAULT_SETTINGS);
    ws().setActive("c");
    const saved = workspaceSnapshot(ws(), { textOf: () => "", isDirty: () => false });
    expect([saved.active, saved.tabs.map((t) => t.id)]).toEqual(["c", ["a", "c", "b", "d"]]);
    const before = { notebooks: ws().notebooks, shown: ws().shown };
    useWorkspace.setState(useWorkspace.getInitialState(), true);
    ws().restore(saved, [config("c")], DEFAULT_SETTINGS);
    expect({ notebooks: ws().notebooks, shown: ws().shown }).toEqual(before);
    ws().showDatabase({ connectionId: "c", database: "reporting" });
    ws().restore(workspaceSnapshot(ws(), { textOf: () => "", isDirty: () => false }), [config("c")], DEFAULT_SETTINGS);
    expect(ws().shown).toEqual({ kind: "start", db: null });
  });

  it("keeps each cell in one notebook on its database, and puts the cells no notebook claims into the one for their database", () => {
    const [a, b, c] = oldWorkspace.tabs;
    const mine = { id: "n1", title: "Mine", db: shop, cells: ["a", "closed"], active: "closed" };
    const pg = { id: "n2", title: "Postgres", db: postgres, cells: ["a", "b"], active: "a" };
    ws().restore({ ...oldWorkspace, tabs: [a!, b!, c!], notebooks: [mine, pg], shown: "n1" }, [config("c")], DEFAULT_SETTINGS);
    expect(shape()).toEqual([
      ["Mine", ["a", "c"], "a"],
      ["Postgres", ["b"], "b"],
    ]);
    expect(ws().shown).toEqual({ kind: "notebook", id: "n1" });
  });

  it("adds a cell to the notebook for its database, after the cell it names, and shows that notebook", () => {
    ws().addTab(cell("a"));
    ws().addTab(cell("b", postgres));
    expect(shownDb(ws())).toEqual(postgres);
    ws().addTab(cell("c"));
    ws().addTab(cell("d"), "a");
    expect(shape()).toEqual([
      ["shop", ["a", "d", "c"], "d"],
      ["postgres", ["b"], "b"],
    ]);
    expect(activeId(ws())).toBe("d");
  });

  it("switches notebooks by database, remembers each one's active cell, and offers the start commands on a database without one", () => {
    ws().addTab(cell("a"));
    ws().addTab(cell("b"));
    ws().setActive("a");
    ws().addTab(cell("p", postgres));
    const tabs = ws().tabs;
    ws().showDatabase(shop);
    expect(activeId(ws())).toBe("a");
    ws().showDatabase({ connectionId: "c", database: "reporting" });
    expect([ws().shown, activeId(ws())]).toEqual([{ kind: "start", db: { connectionId: "c", database: "reporting" } }, null]);
    ws().setActive("p");
    expect(shownDb(ws())).toEqual(postgres);
    expect(ws().tabs).toBe(tabs);
  });

  it("drops a notebook with its last cell and shows the start commands on its database", () => {
    ws().addTab(cell("a"));
    ws().addTab(cell("b"));
    ws().addTab(cell("p", postgres));
    ws().closeTab("p");
    expect([ws().shown, shape()]).toEqual([{ kind: "start", db: postgres }, [["shop", ["a", "b"], "b"]]]);
    ws().setActive("a");
    ws().closeTab("a");
    expect([shownDb(ws()), activeId(ws())]).toEqual([shop, "b"]);
  });

  it("moves cells with no connection into the notebook of the connection saved for them, and keeps showing the active one", () => {
    ws().addTab(cell("free", null));
    ws().addTab(cell("other", null));
    ws().setActive("free");
    ws().upsertConfig(config("c"));
    expect(shape()).toEqual([["shop", ["free", "other"], "free"]]);
    expect(activeId(ws())).toBe("free");
  });

  it("follows a cell that moves with its connection's database into that database's notebook", () => {
    ws().upsertConfig(config("c"));
    ws().addTab(cell("query"));
    ws().addTab({ ...cell("orders"), table: { schema: "public", table: "orders", page: 0, query: EMPTY_QUERY } });
    ws().setActive("query");
    ws().upsertConfig({ ...config("c"), database: "shop_v2" });
    expect(shape()).toEqual([
      ["shop", ["orders"], "orders"],
      ["shop_v2", ["query"], "query"],
    ]);
    expect(activeId(ws())).toBe("query");
  });
});
