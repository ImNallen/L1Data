import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, expect, it } from "vitest";
import { buildCatalogView, useCatalog } from "../../state/catalog";
import { dbKey } from "../../state/dbKey";
import { useLive } from "../../state/live";
import { useWorkspace } from "../../state/workspace";
import { connectedState, connectionConfig, databaseState } from "../../testing/connections";
import { shopCatalog } from "../../testing/shopCatalog";
import { Sidebar } from "./Sidebar";

const initial = { workspace: { ...useWorkspace.getInitialState() }, live: { ...useLive.getInitialState() }, catalog: { ...useCatalog.getInitialState() } };

afterEach(() => {
  Object.assign(useWorkspace.getInitialState(), initial.workspace);
  Object.assign(useLive.getInitialState(), initial.live);
  Object.assign(useCatalog.getInitialState(), initial.catalog);
});

// Static rendering reads each store's initial state, so the fixtures go there.
function render(treeQuery: string, loaded = true, tint?: { tintConnections: boolean }) {
  const configs = tint ? { p: { ...connectionConfig("p", "prod"), color: "red" }, d: connectionConfig("d", "dev") } : { p: connectionConfig("p", "prod") };
  const settings = { ...initial.workspace.settings, ...tint };
  Object.assign(useWorkspace.getInitialState(), { treeQuery, configs, settings });
  Object.assign(useLive.getInitialState(), { connections: { p: connectedState("p", [databaseState("shop", true, ["public"])]) } });
  const view = buildCatalogView(shopCatalog("p", 1));
  Object.assign(useCatalog.getInitialState(), { views: loaded ? { [dbKey(view.catalog.db)]: view } : {} });
  return renderToStaticMarkup(<Sidebar hosted={null} />);
}

/** Each tree row as its path, with "+" or "-" when it has an open or closed chevron. */
const rows = (html: string) =>
  [...html.matchAll(/data-testid="tree-node-([^"]+)" role="treeitem"( aria-expanded="(true|false)")?/g)].map(
    ([, path, , open]) => `${path}${open === undefined ? "" : open === "true" ? " +" : " -"}`,
  );

it("narrows the tree to the matches, every node open and each matching column right under its table", () => {
  const html = render("order");
  expect(rows(html)).toEqual([
    "prod +",
    "prod/shop +",
    "prod/shop/public +",
    "prod/shop/public/order_items +",
    "prod/shop/public/order_items/#columns/order_id",
    "prod/shop/public/orders",
    "prod/shop/public/payments +",
    "prod/shop/public/payments/#columns/order_id",
    "prod/shop/public/shipments +",
    "prod/shop/public/shipments/#columns/order_id",
    "prod/shop/public/paid_orders",
    "prod/shop/public/#functions +",
    "prod/shop/public/#functions/order_count",
    "prod/shop/public/#functions/touch_order",
  ]);
  expect(html).toMatch(/data-testid="sidebar-search-count"[^>]*>8</);
  expect(html).toContain('paid_<mark class="rounded-[2px] bg-amber/20 text-inherit">order</mark>s');
  expect(html).toContain('aria-label="Clear search"');
});

it("says why the tree is empty while a search finds nothing", () => {
  const note = (html: string) => /data-testid="tree-search-empty"[^>]*>(.*?)<\/p>/.exec(html)?.[1];
  expect(note(render("zzqx"))).toBe("No matches for “zzqx”.");
  expect(note(render("order", false))).toBe("Connect to a database to search its catalog.");
});

it("draws the whole tree with no count or clear button while the search box is empty", () => {
  const html = render("");
  expect(rows(html)).toEqual(["prod -"]);
  expect(html).not.toContain('data-testid="sidebar-search-count"');
  expect(html).toMatch(/<input[^>]*data-testid="sidebar-search"[^>]*value=""/);
});

/** The markup inside the element marked `data-testid={id}`, up to its closing tag. */
function inside(html: string, id: string): string | undefined {
  const start = html.indexOf(`data-testid="${id}"`);
  if (start < 0) return undefined;
  let depth = 0;
  for (const m of html.slice(start).matchAll(/<(\/?)div\b/g)) {
    depth += m[1] ? -1 : 1;
    if (depth < 0) return html.slice(start, start + m.index);
  }
}

const RED_DOT = "background-color:#ef4444";

it("shades a colored connection and every row open under it as one block, without its dot, while tinting is on", () => {
  const searched = render("order", true, { tintConnections: true });
  expect(searched).toMatch(/data-testid="conn-tint-prod"[^>]*style="background-color:color-mix\(in srgb, #ef4444 10%, transparent\)"/);
  expect(rows(inside(searched, "conn-tint-prod") ?? "")).toEqual(rows(searched));
  expect(rows(searched)).toHaveLength(14);

  const whole = render("", true, { tintConnections: true });
  expect(rows(whole)).toEqual(["dev -", "prod -"]);
  expect(rows(inside(whole, "conn-tint-prod") ?? "")).toEqual(["prod -"]);
  expect(whole).not.toContain("conn-tint-dev");
  expect(whole).not.toContain(RED_DOT);
});

it("keeps the color dot and draws no shading while tinting is off", () => {
  const html = render("", true, { tintConnections: false });
  expect(rows(html)).toEqual(["dev -", "prod -"]);
  expect(html).not.toContain("conn-tint-");
  expect(html).toContain(RED_DOT);
});

it("keeps long relation suffixes and search marks visible with the full catalog path", () => {
  render("orders");
  const catalog = shopCatalog("p", 1);
  const schema = catalog.schemas.find((s) => s.name === "public");
  const relation = schema?.relations[0];
  if (!relation) throw new Error("The shop fixture needs a relation");
  relation.name = "customer_orders_archived_2026";
  const view = buildCatalogView(catalog);
  Object.assign(useCatalog.getInitialState(), { views: { [dbKey(catalog.db)]: view } });
  const html = renderToStaticMarkup(<Sidebar hosted={null} />);
  const item = inside(html, "tree-node-prod/shop/public/customer_orders_archived_2026");
  expect(item).toContain('title="prod / shop / public / customer_orders_archived_2026"');
  expect(item).toContain('aria-description="prod / shop / public / customer_orders_archived_2026"');
  expect(item).toContain('<span class="sr-only">customer_orders_archived_2026</span>');
  expect(item).toContain(">rchived_2026</bdi>");
  expect(item).toContain(">orders</mark>");
});
