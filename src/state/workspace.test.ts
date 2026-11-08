import { afterEach, describe, expect, it } from "vitest";
import type { ConnectionConfig, FileSaved } from "../ipc/bindings";
import { EMPTY_QUERY, tabFromSnapshot, useWorkspace, workspaceSnapshot } from "./workspace";

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
    expect(useWorkspace.getState().order).toEqual(["a", "c", "b", "d"]);
    expect(useWorkspace.getState().active).toBe("d");
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
