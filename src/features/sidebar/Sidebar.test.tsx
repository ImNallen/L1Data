// @vitest-environment jsdom
import { act, cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ConsoleState } from "../../ipc/bindings";
import * as runActions from "../editor/runActions";
import { dropEditor } from "../editor/editorStates";
import { buildCatalogView, useCatalog } from "../../state/catalog";
import { dbKey } from "../../state/dbKey";
import { useTableEdits } from "../../state/tableEdits";
import { useLive } from "../../state/live";
import { activeId, useWorkspace } from "../../state/workspace";
import { connectedState, connectionConfig, databaseState } from "../../testing/connections";
import { shopCatalog } from "../../testing/shopCatalog";
import { Sidebar } from "./Sidebar";

beforeEach(() => {
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockImplementation(function (this: HTMLElement) {
    return this.hasAttribute("data-index") ? 28 : 600;
  });
});

beforeEach(() => {
  Element.prototype.scrollIntoView = () => {};
  const view = buildCatalogView(shopCatalog("p", 1));
  useWorkspace.setState({ configs: { p: connectionConfig("p", "Postgres") } });
  useLive.setState({ connections: { p: connectedState("p", [databaseState("shop", true, ["public"])]) } });
  useCatalog.setState({ views: { [dbKey(view.catalog.db)]: view } });
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  for (const id of Object.keys(useWorkspace.getState().tabs)) dropEditor(id);
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
  useCatalog.setState(useCatalog.getInitialState(), true);
  useTableEdits.setState(useTableEdits.getInitialState(), true);
});

it("splits databases from their objects and gives each tree its own keyboard stop", () => {
  render(<Sidebar hosted={null} />);
  const databases = within(screen.getByRole("tree", { name: "Connections" }));
  const objects = within(screen.getByRole("tree", { name: "Database objects" }));
  expect(databases.getByTestId("tree-node-Postgres/shop")).toBeTruthy();
  expect(databases.queryByTestId("tree-node-Postgres/shop/public/orders")).toBeNull();
  expect(objects.getByTestId("tree-node-Postgres/shop/public/orders")).toBeTruthy();
  expect(objects.queryByTestId("tree-node-Postgres")).toBeNull();
  for (const tree of [databases, objects]) expect(tree.getAllByRole("treeitem").filter((item) => item.tabIndex === 0)).toHaveLength(1);
});

it("searches database names separately from object names", async () => {
  const user = userEvent.setup();
  render(<Sidebar hosted={null} />);
  await user.type(screen.getByTestId("sidebar-search"), "shop");
  expect(within(screen.getByRole("tree", { name: "Connections" })).getByText("shop")).toBeTruthy();
  await user.type(screen.getByTestId("object-search"), "orders");
  expect(within(screen.getByRole("tree", { name: "Database objects" })).getByTestId("tree-node-Postgres/shop/public/orders")).toBeTruthy();
  expect(screen.getByTestId("sidebar-search")).toHaveProperty("value", "shop");
});

it("switches table and view categories without losing access to routines", async () => {
  const user = userEvent.setup();
  render(<Sidebar hosted={null} />);
  expect(screen.queryByTestId("tree-node-Postgres/shop/public/paid_orders")).toBeNull();
  await user.click(screen.getByRole("button", { name: /^Views/ }));
  expect(screen.getByTestId("tree-node-Postgres/shop/public/paid_orders")).toBeTruthy();
  expect(screen.queryByTestId("tree-node-Postgres/shop/public/orders")).toBeNull();
  await user.click(screen.getByRole("button", { name: "More" }));
  expect(screen.getByTestId("tree-node-Postgres/shop/public/#functions")).toBeTruthy();
});

it("keeps updates visible and keyboard reachable at the right end of the sidebar header", async () => {
  useLive.setState({
    updates: {
      rev: 1,
      track: "Stable",
      currentVersion: "0.1.0",
      checkedAtMs: null,
      status: { kind: "Available", version: "0.2.0", downgrade: false, changes: [] },
    },
  });
  const user = userEvent.setup();
  render(<Sidebar hosted={null} />);
  const header = screen.getByTestId("sidebar-header");
  const update = within(header).getByRole("button", { name: "Download L1 Data 0.2.0" });
  expect(header.lastElementChild).toBe(update);
  screen.getByRole("button", { name: "Workspace options" }).focus();
  await user.tab();
  expect(document.activeElement).toBe(update);
});

function addSecondDatabase() {
  const view = buildCatalogView(shopCatalog("q", 1));
  useWorkspace.setState((s) => ({ configs: { ...s.configs, q: connectionConfig("q", "Alpha") } }));
  useLive.setState((s) => ({ connections: { ...s.connections, q: connectedState("q", [databaseState("shop", true, ["public"])]) } }));
  useCatalog.setState((s) => ({ views: { ...s.views, [dbKey(view.catalog.db)]: view } }));
}

it("opens the first visible search match in the selected database and category", async () => {
  addSecondDatabase();
  const open = vi.spyOn(runActions, "openTableTab").mockResolvedValue(null);
  const user = userEvent.setup();
  render(<Sidebar hosted={null} />);
  await user.type(screen.getByTestId("object-search"), "order");
  await user.keyboard("{Enter}");
  expect(open).toHaveBeenLastCalledWith({ db: { connectionId: "p", database: "shop" }, schema: "public", relation: "order_items" });
  await user.click(screen.getByRole("button", { name: /^Views/ }));
  expect(screen.queryByTestId("tree-node-Postgres/shop/public/orders")).toBeNull();
  await user.click(screen.getByTestId("object-search"));
  await user.keyboard("{Enter}");
  expect(open).toHaveBeenLastCalledWith({ db: { connectionId: "p", database: "shop" }, schema: "public", relation: "paid_orders" });
});

it("shows the picked database's notebook, or its start commands, without moving a cell or its staged edits", async () => {
  addSecondDatabase();
  const shop = { connectionId: "p", database: "shop" };
  const id = runActions.openSqlTab({ db: shop });
  useTableEdits.getState().addRow(id);
  const user = userEvent.setup();
  render(<Sidebar hosted={null} />);
  await user.click(screen.getByTestId("tree-node-Alpha/shop"));
  expect(useWorkspace.getState().shown).toEqual({ kind: "start", db: { connectionId: "q", database: "shop" } });
  expect(useWorkspace.getState().tabs[id]?.console.db).toEqual(shop);
  expect(useTableEdits.getState().byTab[id]?.inserts).toHaveLength(1);
  expect(screen.getByTestId("tree-node-Alpha/shop/public/orders")).toBeTruthy();
  expect(screen.queryByTestId("tree-node-Postgres/shop/public/orders")).toBeNull();
  await user.click(screen.getByTestId("tree-node-Postgres/shop"));
  expect(activeId(useWorkspace.getState())).toBe(id);
  expect(screen.getByTestId("tree-node-Postgres/shop/public/orders")).toBeTruthy();
});

it("does not open a search match when the active cell has no database", async () => {
  runActions.openSqlTab({ db: null });
  const open = vi.spyOn(runActions, "openTableTab").mockResolvedValue(null);
  const user = userEvent.setup();
  render(<Sidebar hosted={null} />);
  await user.type(screen.getByTestId("object-search"), "orders{Enter}");
  expect(open).not.toHaveBeenCalled();
  expect(screen.queryByRole("tree", { name: "Database objects" })).toBeNull();
});

it("marks a database whose notebook is still running or holds a transaction", () => {
  useLive.setState({ connections: { p: connectedState("p", [databaseState("shop", true, ["public"]), databaseState("postgres", true, [])]) } });
  const shop = runActions.openSqlTab({ db: { connectionId: "p", database: "shop" } });
  const postgres = runActions.openSqlTab({ db: { connectionId: "p", database: "postgres" } });
  const console = (tabId: string, patch: Partial<ConsoleState>): ConsoleState => ({
    tabId,
    rev: 1,
    config: { connectionId: "p", database: "shop", schema: null, mode: "Manual" },
    session: { kind: "Open", backendPid: 1 },
    txn: "Idle",
    txnDepth: null,
    work: null,
    export: null,
    import: null,
    run: { kind: "Idle" },
    ...patch,
  });
  useLive.setState({ consoles: { [shop]: console(shop, { work: "Run" }), [postgres]: console(postgres, { txn: "Active" }) } });
  render(<Sidebar hosted={null} />);
  expect(screen.getByTestId("notebook-marker-Postgres/shop").textContent).toBe("Running");
  expect(screen.getByTestId("notebook-marker-Postgres/postgres").textContent).toBe("Open transaction");
  act(() => useLive.setState({ consoles: {} }));
  expect(screen.queryByTestId("notebook-marker-Postgres/shop")).toBeNull();
});
