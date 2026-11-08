import { mockIPC, clearMocks } from "@tauri-apps/api/mocks";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { useLive } from "../../state/live";
import { allCells, shownDb, useWorkspace } from "../../state/workspace";
import { connectedState, connectionConfig, databaseState } from "../../testing/connections";
import { dropEditor, textOf } from "../editor/editorStates";
import { openSqlTab } from "../editor/runActions";
import { activateDestination } from "./navigationActions";

const shop = { connectionId: "navigation-c", database: "shop" };
const reporting = { ...shop, database: "reporting" };
const ws = () => useWorkspace.getState();

beforeEach(() => {
  vi.stubGlobal("window", { crypto });
  ws().upsertConfig(connectionConfig(shop.connectionId, "Local"));
  useLive.setState({
    connections: { [shop.connectionId]: connectedState(shop.connectionId, [databaseState("shop", true, ["public"]), databaseState("reporting", false, [])]) },
  });
});

afterEach(() => {
  for (const id of Object.keys(ws().tabs)) dropEditor(id);
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
  clearMocks();
  vi.unstubAllGlobals();
});

it("switches to the database's notebook at the cell it left active, and leaves every session target intact", async () => {
  const calls: string[] = [];
  mockIPC((cmd) => calls.push(cmd));
  const first = openSqlTab({ db: reporting });
  openSqlTab({ db: reporting });
  ws().setActive(first);
  const original = ws().tabs;
  ws().open({ kind: "navigation" });
  expect(await activateDestination({ kind: "database", db: reporting })).toBe(first);
  expect(ws().tabs).toBe(original);
  openSqlTab({ db: shop });
  expect(shownDb(ws())).toEqual(shop);
  expect(await activateDestination({ kind: "database", db: reporting })).toBe(first);
  expect(shownDb(ws())).toEqual(reporting);
  expect(calls).toEqual([]);
});

it("opens a query in the chosen hidden database without retargeting the current tab", async () => {
  const current = openSqlTab({ db: shop, text: "begin;" });
  const original = ws().tabs[current];
  const opened = await activateDestination({ kind: "database", db: reporting });
  expect(opened).not.toBeNull();
  expect(opened && ws().tabs[opened]?.console.db).toEqual(reporting);
  expect(ws().tabs[current]).toBe(original);
  expect(textOf(current)).toBe("begin;");
});

it("refuses a removed database instead of opening a tab on a default database", async () => {
  const original = ws().tabs;
  useLive.setState({ connections: {} });
  expect(await activateDestination({ kind: "database", db: reporting })).toBeNull();
  expect(ws().tabs).toBe(original);
  expect(ws().toast?.text).toContain("no longer available");
  ws().removeConfig(shop.connectionId);
  expect(await activateDestination({ kind: "database", db: shop })).toBeNull();
  expect(ws().tabs).toBe(original);
});

it("rejects a stale locator that openSqlTab would resolve to a different database", async () => {
  const state = connectedState(shop.connectionId, [databaseState("shop", true, []), databaseState("other", false, [])]);
  if (state.status.kind !== "Connected") throw new Error("Expected a connected fixture");
  state.status.instance = { ...state.status.instance, configuredDatabase: "other", home: "shop" };
  useLive.setState({ connections: { [state.id]: state } });
  expect(await activateDestination({ kind: "database", db: { ...shop, database: "other" } })).toBeNull();
  expect(allCells(ws())).toEqual([]);
});

it("uses the engine table action and closes the popup before its asynchronous error or question", async () => {
  const ref = { db: reporting, schema: "public", relation: "navigation_orders" };
  mockIPC((cmd, args) => {
    expect(cmd).toBe("table_script");
    expect(args).toEqual({ db: ref.db, schema: ref.schema, table: ref.relation });
    expect(ws().overlay.kind).toBe("none");
    ws().open({ kind: "history" });
    return { text: "engine browse template", acceptedTexts: ["engine browse template"] };
  });
  ws().open({ kind: "navigation" });
  const opened = await activateDestination({ kind: "relation", ref });
  expect(opened && ws().tabs[opened]?.console.db).toEqual(reporting);
  expect(opened && textOf(opened)).toBe("engine browse template");
  expect(ws().overlay.kind).toBe("history");
});
