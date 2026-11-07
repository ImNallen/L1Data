// @vitest-environment jsdom
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeAll, beforeEach, expect, it, vi } from "vitest";
import { buildCatalogView, useCatalog } from "../../state/catalog";
import { dbKey } from "../../state/dbKey";
import { useLive } from "../../state/live";
import { useWorkspace } from "../../state/workspace";
import { connectedState, connectionConfig, databaseState } from "../../testing/connections";
import { shopCatalog } from "../../testing/shopCatalog";
import { Sidebar } from "./Sidebar";

beforeAll(() => {
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
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
  useCatalog.setState(useCatalog.getInitialState(), true);
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

it("keeps updates keyboard reachable in workspace options", async () => {
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
  await user.click(screen.getByRole("button", { name: "Workspace options" }));
  await user.keyboard("{End}");
  expect(document.activeElement).toBe(screen.getByRole("menuitem", { name: "Download L1 Data 0.2.0" }));
});
