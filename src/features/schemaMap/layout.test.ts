import { describe, expect, it } from "vitest";
import type { SchemaMap } from "../../ipc/bindings";
import { SHOP_SCHEMA_MAP } from "../../testing/shopSchemaMap";
import { layoutMap, routeLinks, withMoves, type CardMetrics, type MapLayout } from "./layout";
import { toModel, type ErModel, type MapColumns } from "./model";
import type { Rect } from "./viewport";

const METRICS: CardMetrics = { width: 232, header: 34, row: 20, bottomPadding: 6, layerGap: 96, cardGap: 28, componentGap: 72, frameInset: 28 };
const LAYER = METRICS.width + METRICS.layerGap;

const shop = toModel(SHOP_SCHEMA_MAP);
const oidOf = (model: ErModel, name: string) => [...model.tables.values()].find((t) => t.name === name)!.oid;
const card = (model: ErModel, layout: MapLayout, name: string) => layout.cards.get(oidOf(model, name))!;
const byName = (model: ErModel, layout: MapLayout) =>
  Object.fromEntries([...layout.cards].map(([oid, r]) => [model.tables.get(oid)!.name, [r.x, r.y, r.width, r.height]]));
const overlaps = (a: Rect, b: Rect) => a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height;

function schema(tables: Record<string, string[]>, links: string[], inferred: string[] = []): SchemaMap {
  const names = Object.keys(tables);
  const oid = (name: string) => 100 + names.indexOf(name);
  const link = (spec: string, i: number, basis: "Declared" | "Inferred") => {
    const [from, to] = spec.split(" -> ") as [string, string];
    const [source, sourceColumn] = from.split(".") as [string, string];
    const [target, targetColumn] = to.split(".") as [string, string];
    return {
      id: basis === "Declared" ? `k${i}` : `i${oid(source)}.${i}`,
      basis,
      name: basis === "Declared" ? `fk${i}` : null,
      source: oid(source),
      target: oid(target),
      columns: [{ source: sourceColumn, target: targetColumn }],
      enabled: basis === "Declared",
      validated: basis === "Declared",
    };
  };
  return {
    db: { connectionId: "c", database: "d" },
    catalogVersion: 1,
    schema: "s",
    tables: names.map((name) => ({
      oid: oid(name),
      schema: "s",
      name,
      kind: "Table",
      rowEstimate: null,
      partitions: 0,
      external: false,
      columns: tables[name]!.map((c, i) => ({ attnum: i + 1, name: c, typeDisplay: "bigint", category: "Integer", notNull: true, pk: i === 0 })),
    })),
    links: [...links.map((l, i) => link(l, i, "Declared")), ...inferred.map((l, i) => link(l, i, "Inferred"))],
    keysTruncated: false,
    partitionsTruncated: false,
    omittedTables: 0,
  };
}

describe("layoutMap on the shop schema", () => {
  const all = layoutMap(shop, "All", METRICS);
  const keys = layoutMap(shop, "Keys", METRICS);

  it("puts referenced tables left of the tables that reference them", () => {
    const layers = Object.fromEntries(
      [...shop.tables.values()].filter((t) => !shop.isolated.includes(t.oid)).map((t) => [t.name, all.cards.get(t.oid)!.x / LAYER]),
    );
    expect(layers).toEqual({
      categories: 0,
      customers: 0,
      addresses: 1,
      products: 1,
      suppliers: 1,
      inventory: 2,
      orders: 2,
      reviews: 2,
      order_audit: 3,
      order_events: 3,
      order_items: 3,
      payments: 3,
      shipments: 3,
    });
  });

  it("sizes each card by the rows the column mode shows", () => {
    expect(card(shop, all, "orders").height).toBe(34 + 9 * 20 + 6);
    expect(card(shop, keys, "orders").height).toBe(34 + 3 * 20 + 6);
    expect(card(shop, keys, "customers").height).toBe(34 + 1 * 20 + 6);
  });

  it.each([
    ["All", all],
    ["Keys", keys],
  ] as const)("never overlaps two cards in %s mode", (_, layout) => {
    const rects = [...layout.cards.values()];
    for (const [i, a] of rects.entries()) for (const b of rects.slice(i + 1)) expect(overlaps(a, b)).toBe(false);
  });

  it("draws the reviewed picture in All mode", () => {
    expect(byName(shop, all)).toEqual({
      addresses: [328, 165, 232, 140],
      categories: [0, 413, 232, 100],
      customers: [0, 165, 232, 140],
      inventory: [656, 497, 232, 100],
      order_audit: [984, 0, 232, 120],
      order_events: [984, 148, 232, 100],
      order_items: [984, 612, 232, 140],
      orders: [656, 249, 232, 220],
      payments: [984, 276, 232, 140],
      products: [328, 413, 232, 140],
      reviews: [656, 54, 232, 140],
      shipments: [984, 444, 232, 120],
      suppliers: [328, 581, 232, 100],
      coupons: [28, 858, 232, 100],
      tickets: [288, 858, 232, 140],
    });
    expect(all.unlinkedFrame).toEqual({ x: 0, y: 824, width: 548, height: 202 });
    expect(all.bounds).toEqual({ x: 0, y: 0, width: 1216, height: 1026 });
  });

  it("draws the reviewed picture in Keys mode", () => {
    expect(byName(shop, keys)).toEqual({
      addresses: [328, 121, 232, 80],
      categories: [0, 277, 232, 80],
      customers: [0, 121, 232, 60],
      inventory: [656, 317, 232, 80],
      order_audit: [984, 0, 232, 80],
      order_events: [984, 108, 232, 80],
      order_items: [984, 437, 232, 100],
      orders: [656, 189, 232, 100],
      payments: [984, 216, 232, 80],
      products: [328, 277, 232, 80],
      reviews: [656, 34, 232, 100],
      shipments: [984, 324, 232, 80],
      suppliers: [328, 385, 232, 60],
      coupons: [28, 643, 232, 60],
      tickets: [288, 643, 232, 60],
    });
    expect(keys.unlinkedFrame).toEqual({ x: 0, y: 609, width: 548, height: 122 });
  });

  it("frames the tables without links in name order below the rest", () => {
    const linkedBottom = Math.max(...[...all.cards].filter(([oid]) => !shop.isolated.includes(oid)).map(([, r]) => r.y + r.height));
    const frame = all.unlinkedFrame!;
    expect(frame.y).toBe(linkedBottom + METRICS.componentGap);
    const coupons = card(shop, all, "coupons");
    const tickets = card(shop, all, "tickets");
    expect(coupons.x).toBeLessThan(tickets.x);
    for (const r of [coupons, tickets]) {
      expect(r.y).toBeGreaterThanOrEqual(frame.y + METRICS.header);
      expect(r.y + r.height).toBeLessThanOrEqual(frame.y + frame.height);
      expect(r.x + r.width).toBeLessThanOrEqual(frame.x + frame.width);
    }
  });

  it("is the same for the same reply in any order", () => {
    const shuffled = toModel({
      ...SHOP_SCHEMA_MAP,
      tables: [...SHOP_SCHEMA_MAP.tables].sort((a, b) => b.oid - a.oid),
      links: [...SHOP_SCHEMA_MAP.links].reverse(),
    });
    for (const columns of ["All", "Keys"] as MapColumns[]) {
      const again = layoutMap(shuffled, columns, METRICS);
      expect(again).toEqual(layoutMap(shop, columns, METRICS));
      expect(routeLinks(shuffled, again, columns)).toEqual(routeLinks(shop, layoutMap(shop, columns, METRICS), columns));
    }
  });
});

describe("layoutMap on cycles", () => {
  it("breaks a cycle in name order and still layers every table", () => {
    const model = toModel(schema({ a: ["id", "b_id"], b: ["id", "c_id"], c: ["id", "a_id"] }, ["a.b_id -> b.id", "b.c_id -> c.id", "c.a_id -> a.id"]));
    const layout = layoutMap(model, "All", METRICS);
    expect([card(model, layout, "c").x, card(model, layout, "b").x, card(model, layout, "a").x]).toEqual([0, LAYER, 2 * LAYER]);
    expect(layoutMap(model, "All", METRICS)).toEqual(layout);
  });

  it("keeps a table whose only link is to itself out of the isolated group and loops the link on its right", () => {
    const model = toModel(schema({ tree: ["id", "parent_id"] }, ["tree.parent_id -> tree.id"]));
    const layout = layoutMap(model, "All", METRICS);
    expect(layout.unlinkedFrame).toBeNull();
    expect(layout.cards.get(oidOf(model, "tree"))).toEqual({ x: 0, y: 0, width: 232, height: 80 });
    const [route] = routeLinks(model, layout, "All");
    expect(route!.source).toEqual({ at: { x: 232, y: 64 }, facing: "right", marker: "many" });
    expect(route!.target).toEqual({ at: { x: 232, y: 44 }, facing: "right", marker: "one" });
    expect(route!.path).toBe("M 232 64 C 280 64 280 44 232 44");
  });

  it("lays out a schema with no links as one grid", () => {
    const model = toModel(schema({ a: ["id"], b: ["id"], c: ["id"] }, []));
    const layout = layoutMap(model, "All", METRICS);
    expect(layout.unlinkedFrame).toEqual({ x: 0, y: 0, width: 2 * 28 + 232, height: 34 + 3 * 60 + 2 * 28 + 28 });
    expect([...layout.cards.values()].map((r) => [r.x, r.y])).toEqual([
      [28, 34],
      [28, 122],
      [28, 210],
    ]);
  });
});

describe("withMoves", () => {
  const layout = layoutMap(shop, "All", METRICS);
  const orders = oidOf(shop, "orders");
  const moved = withMoves(layout, new Map([[orders, { x: 2000, y: 40 }]]));

  it("puts a dragged card at its dragged origin and leaves the rest", () => {
    expect(moved.cards.get(orders)).toEqual({ x: 2000, y: 40, width: 232, height: 220 });
    for (const [oid, r] of layout.cards) if (oid !== orders) expect(moved.cards.get(oid)).toEqual(r);
    expect(moved.bounds).toEqual({ x: 0, y: 0, width: 2232, height: 1026 });
  });

  it("drops the bends of links with a moved end", () => {
    expect([...layout.bends.keys()].sort()).toEqual(["k16461", "k16483", "k16546"]);
    expect([...moved.bends.keys()].sort()).toEqual(["k16483", "k16546"]);
  });

  it("routes from the card's new place", () => {
    const route = routeLinks(shop, moved, "All").find((r) => r.id === "k16478")!;
    expect(route.target).toEqual({ at: { x: 2000, y: 40 + 34 + 10 }, facing: "left", marker: "one" });
  });
});

describe("routeLinks", () => {
  const layout = layoutMap(shop, "All", METRICS);
  const routes = new Map(routeLinks(shop, layout, "All").map((r) => [r.id, r]));

  it("runs from the source's column row to the target's, on the facing sides", () => {
    const orders = card(shop, layout, "orders");
    const customers = card(shop, layout, "customers");
    const route = routes.get("k16461")!;
    expect(route.source).toEqual({ at: { x: orders.x, y: orders.y + 34 + 2 * 20 + 10 }, facing: "left", marker: "many" });
    expect(route.target).toEqual({ at: { x: customers.x + 232, y: customers.y + 34 + 10 }, facing: "right", marker: "one" });
  });

  it("crosses each layer a long link spans between that layer's cards", () => {
    const bend = layout.bends.get("k16461")!.points;
    expect(bend.map((p) => p.x)).toEqual([LAYER, LAYER + 232]);
    expect(bend[0]!.y).toBe(bend[1]!.y);
    const crossed = [...layout.cards.values()].filter((r) => r.x === LAYER);
    for (const r of crossed) expect(bend[0]!.y > r.y && bend[0]!.y < r.y + r.height).toBe(false);
    expect(routes.get("k16461")!.path).toContain(`${LAYER + 232} ${bend[0]!.y} C`);
  });

  it("draws a bar at both ends of a one-to-one link", () => {
    const route = routes.get("k16507")!;
    expect([route.source.marker, route.target.marker]).toEqual(["one", "one"]);
  });

  it("dashes inferred links only", () => {
    expect([...routes.values()].filter((r) => r.dashed).map((r) => r.id)).toEqual(["i16386.5", "i16563.2", "i16606.1"]);
  });

  it("fans out links drawn between the same two rows", () => {
    const model = toModel(schema({ child: ["id", "parent_id"], parent: ["id"] }, ["child.parent_id -> parent.id", "child.parent_id -> parent.id"]));
    const [a, b] = routeLinks(model, layoutMap(model, "All", METRICS), "All");
    expect(b!.source.at.y - a!.source.at.y).toBe(6);
    expect(b!.target.at.y - a!.target.at.y).toBe(6);
    expect((a!.source.at.y + b!.source.at.y) / 2).toBe(card(model, layoutMap(model, "All", METRICS), "child").y + 34 + 20 + 10);
  });
});
