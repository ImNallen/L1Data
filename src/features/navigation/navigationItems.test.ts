import { expect, it } from "vitest";
import { buildCatalogView } from "../../state/catalog";
import { dbKey } from "../../state/dbKey";
import { EMPTY_QUERY, type Tab } from "../../state/workspace";
import { connectedState, connectionConfig, databaseState } from "../../testing/connections";
import { shopCatalog } from "../../testing/shopCatalog";
import { cellsState } from "../../testing/notebooks";
import { navigationItems, searchNavigation, type NavigationItem, type NavigationSource } from "./navigationItems";

const shop = { connectionId: "c", database: "shop" };
const reporting = { connectionId: "c", database: "reporting" };
const tab = (id: string, db = shop): Tab => ({ id, title: `${id}.sql`, console: { db, mode: "AutoCommit" }, table: null, file: null });
const source = (): NavigationSource => ({
  ...cellsState([]),
  configs: { c: connectionConfig("c", "Local"), d: connectionConfig("d", "Offline") },
  connections: { c: connectedState("c", [databaseState("shop", true, ["public"]), databaseState("reporting", false, [])]) },
  views: {},
});

it("includes hidden accessible databases, excludes inaccessible databases, and labels disconnected home databases", () => {
  const input = source();
  input.connections.c = connectedState("c", [
    databaseState("shop", true, ["public"]),
    databaseState("reporting", false, []),
    { ...databaseState("blocked", false, []), access: "NoPrivilege" },
  ]);
  const items = navigationItems(input);
  expect(items.map((item) => [item.label, item.context])).toEqual([
    ["shop", "Local / Connected"],
    ["reporting", "Local / Connected"],
    ["shop", "Offline / Disconnected · saved home database"],
  ]);
  expect(searchNavigation(items, "REPORTING local").items.map((item) => item.destination)).toEqual([{ kind: "database", db: reporting }]);
});

it("scopes identical relation names and object IDs by database and connection, without sidebar schema filtering", () => {
  const input = source();
  input.connections.d = connectedState("d", [databaseState("shop", true, ["public"])]);
  for (const db of [shop, reporting, { connectionId: "d", database: "shop" }]) {
    const catalog = { ...shopCatalog(db.connectionId, 1), db };
    input.views[dbKey(db)] = buildCatalogView(catalog);
  }
  const items = navigationItems(input);
  const orders = searchNavigation(items, "orders").items.filter((item) => item.label === "orders");
  expect(orders.map((item) => item.context)).toEqual(["Local / shop / public", "Local / reporting / public", "Offline / shop / public"]);
  expect(new Set(orders.map((item) => item.id)).size).toBe(3);
  expect(searchNavigation(items, "reporting lab type_zoo").items).toHaveLength(1);
  expect(searchNavigation(items, "reporting paid_orders").items[0]).toMatchObject({ kindLabel: "View", action: "Open view" });
  expect(navigationItems({ ...input, views: {} }).every((item) => item.destination.kind !== "relation")).toBe(true);
});

it("searches query, file and table tabs by context and full file path", () => {
  const input = source();
  const query = tab("query");
  const file: Tab = {
    ...tab("file"),
    file: { health: "ok", binding: { path: "/work/reports/monthly.sql", stamp: "s1", format: { bom: false, crlf: false } } },
  };
  const table: Tab = { ...tab("orders"), table: { schema: "public", table: "orders", page: 0, query: EMPTY_QUERY } };
  Object.assign(input, cellsState([query, file, table]));
  const items = navigationItems(input);
  expect(searchNavigation(items, "LOCAL SHOP query").items[0]?.kindLabel).toBe("Query tab");
  expect(searchNavigation(items, "monthly reports").items[0]).toMatchObject({ kindLabel: "File tab", destination: { kind: "tab", tabId: "file" } });
  expect(searchNavigation(items, "public orders").items[0]?.kindLabel).toBe("Table tab");
});

const item = (label: string, context = ""): NavigationItem => ({
  id: label,
  label,
  context,
  kindLabel: "Query tab",
  action: "Switch to tab",
  destination: { kind: "tab", tabId: label },
});

it("ranks exact names, name prefixes and word prefixes ahead of substring and contextual matches", () => {
  const items = [item("report", "orders"), item("preorders"), item("paid_orders"), item("orders_archive"), item("orders")];
  expect(searchNavigation(items, "  OrDeRs  ").items.map((item) => item.label)).toEqual(["orders", "orders_archive", "paid_orders", "preorders", "report"]);
  expect(searchNavigation(items, "archive absent").total).toBe(0);
});

it("ranks the entire candidate set before bounding the displayed results and reports all matches", () => {
  const items = [...Array.from({ length: 80 }, (_, i) => item(`query-${i}`, "orders")), item("orders")];
  const found = searchNavigation(items, "orders");
  expect(found.total).toBe(81);
  expect(found.items).toHaveLength(60);
  expect(found.items[0]?.label).toBe("orders");
});

it("switches a database with a notebook to it, and opens a new query in one without", () => {
  const input = { ...source(), ...cellsState([tab("first"), tab("report", reporting)], "report") };
  const action = (label: string) => navigationItems(input).find((item) => item.destination.kind === "database" && item.label === label)?.action;
  expect([action("shop"), action("reporting")]).toEqual(["Switch to the shop notebook", "Switch to the reporting notebook"]);
  expect(navigationItems({ ...input, ...cellsState([tab("first")]) }).find((item) => item.label === "reporting")?.action).toBe(
    "Open new query in this database",
  );
  expect(
    navigationItems(input)
      .filter((item) => item.action === "Focus active tab")
      .map((item) => item.label),
  ).toEqual(["report.sql"]);
});
