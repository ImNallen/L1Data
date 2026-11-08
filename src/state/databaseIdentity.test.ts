import { mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ConnectionState, TabSnapshot } from "../ipc/bindings";
import { connectionConfig, connectedState } from "../testing/connections";
import { dropEditor, seedEditor } from "../features/editor/editorStates";
import { browseTemplate, isBrowsing, loadTableOnce, openSqlTab } from "../features/editor/runActions";
import { homeDb, resolveDb } from "./dbKey";
import { useLive } from "./live";
import { allCells, EMPTY_QUERY, shownDb, useWorkspace, workspaceSnapshot } from "./workspace";
import { moveCell } from "../testing/notebooks";

const config = { ...connectionConfig("sql-server", "SQL Server"), databaseKind: "SqlServer" as const, database: "SHOP" };

function connected(configuredDatabase = "SHOP", home = "shop", rev = 1): ConnectionState {
  const state = connectedState(config.id, []);
  if (state.status.kind !== "Connected") throw new Error("Expected a connected fixture");
  return { ...state, rev, status: { ...state.status, instance: { ...state.status.instance, configuredDatabase, home } } };
}

function saved(database: string | null, table = false, connectionId = config.id): TabSnapshot {
  return {
    id: crypto.randomUUID(),
    title: table ? "orders" : "query.sql",
    text: table ? "SELECT * FROM [dbo].[orders]" : "SELECT DB_NAME()",
    console: { connectionId, database, schema: null, mode: "AutoCommit" },
    table: table ? { schema: "dbo", table: "orders", page: 3, query: EMPTY_QUERY } : null,
    file: null,
  };
}

beforeEach(() => {
  vi.stubGlobal("window", { crypto });
});

afterEach(() => {
  for (const id of Object.keys(useWorkspace.getState().tabs)) dropEditor(id);
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
  vi.unstubAllGlobals();
});

it("resolves only the exact locator accepted by this connected instance", () => {
  const state = connected();
  expect(homeDb(config, state)).toEqual({ connectionId: config.id, database: "shop" });
  expect(homeDb(config)).toEqual({ connectionId: config.id, database: "SHOP" });
  for (const database of ["shop", "Shop", "sHoP", "reporting", "REPORTING"]) {
    const ref = { connectionId: config.id, database };
    expect(resolveDb(ref, state)).toBe(ref);
  }
  const other = { connectionId: "other", database: "SHOP" };
  expect(resolveDb(other, state)).toBe(other);
  expect(homeDb({ ...config, database: "reporting" }, state).database).toBe("reporting");
  expect(homeDb({ ...config, database: "Shop" }, connected("Shop", "Shop")).database).toBe("Shop");
});

it("resolves restored home query and table targets and saves their canonical identities", () => {
  const tabs = [saved(null), saved("SHOP", true), saved("shop"), saved("Shop"), saved("reporting"), saved("SHOP", false, "other")];
  const ws = useWorkspace.getState();
  ws.restore({ tabs, active: tabs[0]!.id, layout: ws.layout, notebooks: [], shown: null }, [config, { ...config, id: "other" }], ws.settings);
  const state = connected();
  useLive.getState().apply({ kind: "ConnectionChanged", ...state });
  ws.resolveDatabases(useLive.getState().connections);
  const current = useWorkspace.getState();
  expect(allCells(current).map((id) => current.tabs[id]?.console.db?.database)).toEqual(["shop", "shop", "shop", "Shop", "reporting", "SHOP"]);
  expect(current.tabs[tabs[1]!.id]?.table).toEqual(tabs[1]!.table);
  expect(workspaceSnapshot(current, { textOf: () => "saved text", isDirty: () => false }).tabs.map((tab) => tab.console.database)).toEqual([
    "shop",
    "shop",
    "shop",
    "Shop",
    "reporting",
    "SHOP",
  ]);
  const shopNotebooks = Object.values(current.notebooks).filter((n) => n.db?.connectionId === config.id && n.db.database === "shop");
  expect(shopNotebooks.map((n) => n.cells)).toEqual([[tabs[0]!.id, tabs[1]!.id, tabs[2]!.id]]);
  expect(current.configs[config.id]?.database).toBe("SHOP");
  expect(useLive.getState().connections[config.id]).toEqual(state);
  ws.resolveDatabases(useLive.getState().connections);
  expect(useWorkspace.getState().tabs).toBe(current.tabs);
});

it("keeps a saved-config edit separate from the old connected home until reconnect", () => {
  useLive.getState().apply({ kind: "ConnectionChanged", ...connected() });
  const ws = useWorkspace.getState();
  ws.upsertConfig(config);
  const query = openSqlTab();
  const table = saved("shop", true);
  ws.addTab({
    id: table.id,
    title: "orders",
    console: { db: { connectionId: config.id, database: "shop" }, mode: "AutoCommit" },
    table: table.table,
    file: null,
  });
  ws.upsertConfig({ ...config, database: "reporting" });
  ws.resolveDatabases(useLive.getState().connections);
  expect(useWorkspace.getState().tabs[query]?.console.db?.database).toBe("reporting");
  expect(useWorkspace.getState().tabs[table.id]?.console.db?.database).toBe("shop");
  ws.setActive(query);
  const next = openSqlTab();
  expect(useWorkspace.getState().tabs[next]?.console.db?.database).toBe("reporting");
  useLive.getState().apply({ kind: "ConnectionChanged", ...connected("reporting", "reporting", 2) });
  ws.resolveDatabases(useLive.getState().connections);
  expect(useWorkspace.getState().tabs[table.id]?.console.db?.database).toBe("shop");
});

it("uses canonical targets for new queries, active defaults and connection selection", () => {
  const ws = useWorkspace.getState();
  ws.upsertConfig(config);
  useLive.getState().apply({ kind: "ConnectionChanged", ...connected() });
  const fresh = openSqlTab();
  expect(useWorkspace.getState().tabs[fresh]?.console.db?.database).toBe("shop");
  moveCell(fresh, { connectionId: config.id, database: "SHOP" });
  const next = openSqlTab();
  expect(useWorkspace.getState().tabs[next]?.console.db?.database).toBe("shop");
  ws.showDatabase(homeDb(config, useLive.getState().connections[config.id]));
  expect(shownDb(useWorkspace.getState())?.database).toBe("shop");
});

it("keeps restored table browsing active when the target gains its canonical spelling", async () => {
  const calls: { cmd: string; args: unknown }[] = [];
  mockIPC((cmd, args) => {
    calls.push({ cmd, args });
    if (cmd === "table_script") return { text: "SELECT * FROM [dbo].[orders]", acceptedTexts: ["SELECT * FROM [dbo].[orders]"] };
    if (cmd === "console_run") return { runId: 1, statements: [] };
    return null;
  });
  const ws = useWorkspace.getState();
  const tab = saved("SHOP", true);
  ws.restore({ tabs: [tab], active: tab.id, layout: ws.layout, notebooks: [], shown: null }, [config], ws.settings);
  seedEditor(tab.id, tab.text ?? "");
  await browseTemplate(homeDb(config), "dbo", "orders");
  useLive.getState().apply({ kind: "ConnectionChanged", ...connected() });
  ws.resolveDatabases(useLive.getState().connections);
  const restored = useWorkspace.getState().tabs[tab.id];
  expect(restored && isBrowsing(restored)).toBe(true);
  await loadTableOnce(tab.id);
  await vi.waitFor(() =>
    expect(calls).toContainEqual({
      cmd: "console_run",
      args: { tabId: tab.id, request: { kind: "tablePage", schema: "dbo", table: "orders", page: 3, query: EMPTY_QUERY }, confirmed: null },
    }),
  );
  expect(calls).toContainEqual({
    cmd: "console_configure",
    args: { tabId: tab.id, config: { connectionId: config.id, database: "shop", schema: null, mode: "AutoCommit" } },
  });
});

it("uses only the accepted connection revision when resolving restored targets", () => {
  useLive.getState().apply({ kind: "ConnectionChanged", ...connected("SHOP", "shop", 2) });
  useLive.getState().apply({ kind: "ConnectionChanged", ...connected("SHOP", "unrelated", 1) });
  const ws = useWorkspace.getState();
  const tab = saved("SHOP");
  ws.restore({ tabs: [tab], active: tab.id, layout: ws.layout, notebooks: [], shown: null }, [config], ws.settings);
  ws.resolveDatabases(useLive.getState().connections);
  expect(useWorkspace.getState().tabs[tab.id]?.console.db?.database).toBe("shop");
});
