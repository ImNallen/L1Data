// @vitest-environment jsdom
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeAll, beforeEach, expect, it, vi } from "vitest";
import { buildCatalogView, useCatalog } from "../../state/catalog";
import { dbKey } from "../../state/dbKey";
import { useLive } from "../../state/live";
import { activeTab, useWorkspace } from "../../state/workspace";
import { ConnectionTreeHarness } from "../../testing/ConnectionTreeHarness";
import { connectedState, connectionConfig, databaseState } from "../../testing/connections";
import { shopCatalog } from "../../testing/shopCatalog";
import { useMapViews } from "../schemaMap/views";

beforeAll(() => {
  Element.prototype.scrollIntoView = () => {};
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockImplementation(function (this: HTMLElement) {
    return this.hasAttribute("data-index") ? 28 : 600;
  });
});

beforeEach(() => {
  mockIPC(() => {
    throw "not served in this test";
  });
  const view = buildCatalogView(shopCatalog("p", 1));
  useWorkspace.setState({ configs: { p: connectionConfig("p", "prod") } });
  useLive.setState({ connections: { p: connectedState("p", [databaseState("shop", true, ["public"])]) } });
  useCatalog.setState({ views: { [dbKey(view.catalog.db)]: view } });
});

afterEach(() => {
  cleanup();
  clearMocks();
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
  useCatalog.setState(useCatalog.getInitialState(), true);
  useMapViews.setState(useMapViews.getInitialState(), true);
});

async function openMenu(path: string) {
  const user = userEvent.setup();
  render(<ConnectionTreeHarness />);
  await user.pointer({ keys: "[MouseRight]", target: screen.getByTestId(`tree-node-${path}`) });
  return user;
}

async function addFrom(path: string, item = "tree-menu-schema-map") {
  const user = await openMenu(path);
  await user.click(await screen.findByTestId(item));
  return activeTab(useWorkspace.getState());
}

it("adds a map of the database's default schema from a database row", async () => {
  const tab = await addFrom("prod/shop");
  expect(tab && { title: tab.title, console: tab.console, map: tab.map }).toEqual({
    title: "Schema map",
    console: { db: { connectionId: "p", database: "shop" }, mode: "AutoCommit" },
    map: { columns: "All" },
  });
});

it("adds a map of the schema from a schema row", async () => {
  const tab = await addFrom("prod/shop/public");
  expect(tab && { console: tab.console, map: tab.map }).toEqual({
    console: { db: { connectionId: "p", database: "shop" }, mode: "AutoCommit", schema: "public" },
    map: { columns: "All" },
  });
});

it("maps a table's schema with the table focused from a table row", async () => {
  const tab = await addFrom("prod/shop/public/orders", "tree-menu-show-in-map");
  expect(tab && { console: tab.console, map: tab.map }).toEqual({
    console: { db: { connectionId: "p", database: "shop" }, mode: "AutoCommit", schema: "public" },
    map: { columns: "All" },
  });
  expect(useMapViews.getState().views[tab!.id]?.focus).toBe("public.orders");
});

it("offers no map from a view row, which the map does not draw", async () => {
  await openMenu("prod/shop/public/paid_orders");
  await screen.findByTestId("tree-menu-script-create");
  expect(screen.queryByTestId("tree-menu-show-in-map")).toBeNull();
});
