// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ConsoleState } from "../../ipc/bindings";
import * as connectionActions from "../connections/connectionActions";
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
  await user.click(screen.getByRole("button", { name: /^All/ }));
  expect(screen.getByTestId("tree-node-Postgres/shop/public/#functions")).toBeTruthy();
});

it("puts Queries and new connection at the right of the header, and Settings and Usage at the left of the footer", () => {
  render(<Sidebar hosted={null} />);
  const ids = (testId: string) => [...screen.getByTestId(testId).querySelectorAll("button")].map((b) => b.dataset.testid);
  expect(ids("sidebar-header")).toEqual(["open-queries", "new-connection"]);
  expect(ids("sidebar-footer")).toEqual(["open-settings", "open-usage"]);
});

it("keeps updates visible and keyboard reachable at the right end of the sidebar footer", async () => {
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
  const footer = screen.getByTestId("sidebar-footer");
  const update = within(footer).getByRole("button", { name: "Download L1 Data 0.2.0" });
  expect(footer.lastElementChild).toBe(update);
  screen.getByTestId("open-usage").focus();
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
  expect(screen.queryByTestId("object-search")).toBeNull();
  expect(screen.getByText(/Choose a database to browse/)).toBeTruthy();
  await user.keyboard("orders{Enter}");
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

it("names each connection's environment and reaches its database picker by Tab from the row", async () => {
  const user = userEvent.setup();
  useWorkspace.setState({ configs: { p: { ...connectionConfig("p", "Postgres"), environment: "Production" } } });
  render(<Sidebar hosted={null} />);
  const connection = screen.getByTestId("tree-node-Postgres");
  expect(within(connection).getByText("Production").classList.contains("text-danger")).toBe(true);
  connection.focus();
  await user.tab();
  expect(document.activeElement).toBe(within(connection).getByTestId("db-pill-Postgres"));
  await user.keyboard("{Enter}");
  expect(screen.getByTestId("db-pill-Postgres").getAttribute("aria-expanded")).toBe("true");
});

it("slides the category thumb to the chosen category and shows each count", async () => {
  const user = userEvent.setup();
  render(<Sidebar hosted={null} />);
  const thumb = screen.getByTestId("category-thumb");
  expect(thumb.style.transform).toBe("translateX(0%)");
  expect(screen.getByRole("button", { name: /^Tables/ }).getAttribute("aria-pressed")).toBe("true");
  await user.click(screen.getByRole("button", { name: /^Views/ }));
  expect(thumb.style.transform).toBe("translateX(100%)");
  await user.click(screen.getByRole("button", { name: /^All/ }));
  expect(thumb.style.transform).toBe("translateX(200%)");
  expect(screen.getByRole("button", { name: /^All/ }).getAttribute("aria-pressed")).toBe("true");
});

it("shows a table's row estimate in short form", () => {
  render(<Sidebar hosted={null} />);
  expect(within(screen.getByTestId("tree-node-Postgres/shop/public/addresses")).getByText("18.4k").title).toBe("About 18,400 rows");
});

it("resizes the split from the keyboard, saves it in the layout, and resets it on double-click", async () => {
  const user = userEvent.setup();
  render(<Sidebar hosted={null} />);
  const split = screen.getByRole("separator", { name: "Resize database list" });
  expect(split.getAttribute("aria-valuenow")).toBe("50");
  split.focus();
  await user.keyboard("{ArrowDown}{ArrowDown}");
  expect(useWorkspace.getState().layout.sidebarSplit).toBe(60);
  await user.keyboard("{ArrowUp}");
  expect(useWorkspace.getState().layout.sidebarSplit).toBe(55);
  fireEvent.doubleClick(split);
  expect(useWorkspace.getState().layout.sidebarSplit).toBe(50);
});

it("explains a failed connection under its row, with Retry and Edit connection", async () => {
  const user = userEvent.setup();
  useLive.setState({ connections: { p: { id: "p", rev: 1, status: { kind: "Failed", error: { kind: "Auth", message: "Login failed for user 'sa'." } } } } });
  render(<Sidebar hosted={null} />);
  const card = screen.getByTestId("conn-error-Postgres");
  expect(card.textContent).toContain("Login failed for user 'sa'.");
  expect(within(screen.getByTestId("tree-node-Postgres")).getByText("Failed")).toBeTruthy();
  await user.click(within(card).getByRole("button", { name: "Edit connection…" }));
  expect(useWorkspace.getState().overlay).toEqual({ kind: "connection", editing: "p" });
});

it("opens one picker from the heading, anchors to it, and returns focus on Done and Escape", async () => {
  const user = userEvent.setup();
  render(<Sidebar hosted={null} />);
  const trigger = screen.getByTestId("heading-schema-picker");
  const anchor = trigger.closest<HTMLElement>("[data-picker-anchor]")!;
  vi.spyOn(anchor, "getBoundingClientRect").mockReturnValue({
    left: 0,
    top: 300,
    bottom: 352,
    width: 256,
    right: 256,
    height: 52,
    x: 0,
    y: 300,
    toJSON: () => ({}),
  });
  await user.click(trigger);
  expect(screen.getAllByRole("dialog")).toHaveLength(1);
  const dialog = screen.getByRole("dialog", { name: "Schemas in sidebar" });
  expect(dialog.style.left).toBe("260px");
  expect(dialog.style.top).toBe("300px");
  expect(within(dialog).getByText("shop · Postgres")).toBeTruthy();
  expect(document.activeElement).toBe(screen.getByTestId("picker-filter"));
  await user.click(screen.getByTestId("picker-done"));
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(document.activeElement).toBe(trigger);
  await user.click(trigger);
  await user.keyboard("{Escape}");
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(document.activeElement).toBe(trigger);
});

it("closes the heading picker when the selected database changes", async () => {
  addSecondDatabase();
  const user = userEvent.setup();
  render(<Sidebar hosted={null} />);
  await user.click(screen.getByTestId("heading-schema-picker"));
  expect(screen.getByRole("dialog").textContent).toContain("shop · Postgres");
  act(() => useWorkspace.getState().showDatabase({ connectionId: "q", database: "shop" }));
  expect(screen.queryByRole("dialog")).toBeNull();
  await user.click(screen.getByTestId("heading-schema-picker"));
  expect(screen.getByRole("dialog").textContent).toContain("shop · Alpha");
  await user.keyboard("{Escape}");
});

it("sends visibility edits immediately and keeps the heading reachable through None and restoration", async () => {
  const edit = vi.spyOn(connectionActions, "editView").mockResolvedValue();
  const user = userEvent.setup();
  render(<Sidebar hosted={null} />);
  const trigger = screen.getByTestId("heading-schema-picker");
  await user.click(trigger);
  await user.click(screen.getByTestId("picker-none"));
  expect(edit).toHaveBeenLastCalledWith("p", { kind: "Schemas", database: "shop", preset: "None" });
  act(() => useLive.setState({ connections: { p: connectedState("p", [databaseState("shop", true, [])]) } }));
  expect(screen.getAllByRole("dialog")).toHaveLength(1);
  expect(screen.getByText("No schemas shown")).toBeTruthy();
  expect(screen.getByTestId("heading-schema-picker")).toBe(trigger);
  await user.click(screen.getByTestId("picker-done"));
  expect(document.activeElement).toBe(trigger);
  await user.click(trigger);
  await user.click(screen.getByTestId("picker-default-only"));
  expect(edit).toHaveBeenLastCalledWith("p", { kind: "Schemas", database: "shop", preset: "DefaultOnly" });
  act(() => useLive.setState({ connections: { p: connectedState("p", [databaseState("shop", true, ["public"])]) } }));
  expect(screen.queryByText("No schemas shown")).toBeNull();
  expect(screen.getByTestId("tree-node-Postgres/shop/public/orders")).toBeTruthy();
  await user.click(screen.getByRole("switch", { name: "Show system schemas" }));
  expect(edit).toHaveBeenLastCalledWith("p", { kind: "ShowSystemSchemas", database: "shop", show: true });
  await user.click(screen.getByTestId("picker-done"));
});

it("keeps picker search separate from both sidebar searches and distinguishes an empty catalog", async () => {
  const user = userEvent.setup();
  render(<Sidebar hosted={null} />);
  await user.type(screen.getByTestId("sidebar-search"), "shop");
  await user.type(screen.getByTestId("object-search"), "orders");
  await user.click(screen.getByTestId("heading-schema-picker"));
  await user.type(screen.getByTestId("picker-filter"), "missing-schema");
  expect(screen.getByText("No matches")).toBeTruthy();
  expect(screen.getByTestId("sidebar-search")).toHaveProperty("value", "shop");
  expect(screen.getByTestId("object-search")).toHaveProperty("value", "orders");
  await user.clear(screen.getByTestId("picker-filter"));
  const catalog = { ...shopCatalog("p", 1), schemas: [] };
  act(() => useCatalog.setState({ views: { [dbKey(catalog.db)]: buildCatalogView(catalog) } }));
  expect(screen.getByText("No items available")).toBeTruthy();
  await user.click(screen.getByTestId("picker-done"));
});

it("edits database rows and the system switch without waiting for Done", async () => {
  const edit = vi.spyOn(connectionActions, "editView").mockResolvedValue();
  vi.spyOn(connectionActions, "probeDatabases").mockImplementation(() => {});
  const user = userEvent.setup();
  render(<Sidebar hosted={null} />);
  await user.click(screen.getByTestId("db-pill-Postgres"));
  expect(screen.getAllByRole("dialog")).toHaveLength(1);
  expect(screen.getByRole("dialog", { name: "Databases in sidebar" })).toBeTruthy();
  await user.click(screen.getByTestId("picker-check-shop"));
  expect(edit).toHaveBeenLastCalledWith("p", { kind: "Database", name: "shop", shown: false });
  await user.click(screen.getByTestId("picker-all"));
  expect(edit).toHaveBeenLastCalledWith("p", { kind: "Databases", preset: "All" });
  await user.click(screen.getByRole("switch", { name: "Show system databases" }));
  expect(edit).toHaveBeenLastCalledWith("p", { kind: "ShowSystemDatabases", show: true });
  await user.click(screen.getByTestId("picker-done"));
  expect(document.activeElement).toBe(screen.getByTestId("db-pill-Postgres"));
});

it("shows authoritative schema loading and failure states with catalog retry", async () => {
  const refresh = vi.spyOn(connectionActions, "refreshCatalog").mockResolvedValue();
  useCatalog.setState({ views: {} });
  useLive.setState({ connections: { p: connectedState("p", [{ ...databaseState("shop", true, []), catalog: { kind: "Loading" } }]) } });
  const user = userEvent.setup();
  render(<Sidebar hosted={null} />);
  await user.click(screen.getByTestId("heading-schema-picker"));
  expect(screen.getByText("Loading schemas")).toBeTruthy();
  act(() =>
    useLive.setState({
      connections: {
        p: connectedState("p", [{ ...databaseState("shop", true, []), catalog: { kind: "Failed", message: "Catalog access denied", version: null } }]),
      },
    }),
  );
  expect(screen.getByRole("dialog").textContent).toContain("Catalog access denied");
  await user.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Retry" }));
  expect(refresh).toHaveBeenCalledWith({ connectionId: "p", database: "shop" });
  await user.keyboard("{Escape}");
});
