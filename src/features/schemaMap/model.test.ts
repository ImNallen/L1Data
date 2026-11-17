import { describe, expect, it } from "vitest";
import { SHOP_SCHEMA_MAP } from "../../testing/shopSchemaMap";
import { findMatches, focusOf, toModel, visibleColumns, type ErModel } from "./model";

const shop = toModel(SHOP_SCHEMA_MAP);

function table(model: ErModel, name: string) {
  const oid = model.byKey.get(`public.${name}`);
  if (oid === undefined) throw new Error(`no table ${name}`);
  return model.tables.get(oid)!;
}

const names = (model: ErModel, oids: Iterable<number>) => [...oids].map((oid) => model.tables.get(oid)!.name).sort();
const roles = (name: string) => Object.fromEntries(table(shop, name).columns.map((c) => [c.name, c.role]));

describe("toModel", () => {
  it("orders tables by name and indexes them by schema and name", () => {
    expect([...shop.tables.values()].map((t) => t.name)).toEqual([
      "addresses",
      "categories",
      "coupons",
      "customers",
      "inventory",
      "order_audit",
      "order_events",
      "order_items",
      "orders",
      "payments",
      "products",
      "reviews",
      "shipments",
      "suppliers",
      "tickets",
    ]);
    expect(shop.byKey.get("public.orders")).toBe(16448);
  });

  it("derives each column's role from the links", () => {
    expect(roles("orders")).toMatchObject({ id: "pk", customer_id: "fk", ship_address_id: "fk", status: "plain" });
    expect(roles("inventory")).toEqual({ product_id: "pkfk", supplier_id: "fk", on_hand: "plain" });
    expect(roles("order_audit")).toMatchObject({ id: "pk", order_id: "inferred" });
    expect(roles("products")).toMatchObject({ id: "pk", category_id: "inferred" });
    expect(roles("categories")).toEqual({ id: "pk", name: "plain", parent_id: "fk" });
  });

  it("marks a link one-to-one only when its source columns are the source's whole primary key", () => {
    const oneToOne = [...shop.links.values()].filter((l) => l.oneToOne).map((l) => l.id);
    expect(oneToOne).toEqual(["k16507"]);
  });

  it("lists every link at both ends and a self-reference once", () => {
    expect(shop.incident.get(table(shop, "orders").oid)).toEqual(["k16461", "k16466", "k16478", "k16497", "k16526", "i16563.2", "i16606.1"]);
    expect(shop.incident.get(table(shop, "categories").oid)).toEqual(["k16405", "i16386.5"]);
  });

  it("collects the tables no link touches", () => {
    expect(names(shop, shop.isolated)).toEqual(["coupons", "tickets"]);
  });

  it("does not depend on the reply's order", () => {
    const shuffled = toModel({ ...SHOP_SCHEMA_MAP, tables: [...SHOP_SCHEMA_MAP.tables].reverse(), links: [...SHOP_SCHEMA_MAP.links].reverse() });
    expect(shuffled).toEqual(shop);
    expect([...shuffled.links.keys()]).toEqual([...shop.links.keys()]);
  });
});

describe("visibleColumns", () => {
  const visible = (name: string, mode: "All" | "Keys") => visibleColumns(table(shop, name), mode).map((c) => c.name);

  it("shows primary keys and every link's source and target columns in Keys mode", () => {
    expect(visible("orders", "Keys")).toEqual(["id", "customer_id", "ship_address_id"]);
    expect(visible("addresses", "Keys")).toEqual(["id", "customer_id"]);
    expect(visible("order_events", "Keys")).toEqual(["order_id", "happened_on"]);
    expect(visible("coupons", "Keys")).toEqual(["code"]);
  });

  it("shows every column in attnum order in All mode", () => {
    expect(visible("payments", "All")).toEqual(["id", "order_id", "amount", "method", "paid_at"]);
  });
});

describe("focusOf", () => {
  it("counts the declared references each way and the inferred ones for orders", () => {
    const focus = focusOf(shop, table(shop, "orders").oid)!;
    expect(focus.counts).toEqual({ references: 2, referencedBy: 3, possible: 2 });
    expect(names(shop, focus.related)).toEqual(["addresses", "customers", "order_audit", "order_events", "order_items", "orders", "payments", "shipments"]);
    expect(focus.links).toHaveLength(7);
  });

  it("counts a self-reference both ways", () => {
    expect(focusOf(shop, table(shop, "categories").oid)!.counts).toEqual({ references: 1, referencedBy: 1, possible: 1 });
  });

  it("relates an isolated table only to itself", () => {
    const focus = focusOf(shop, table(shop, "coupons").oid)!;
    expect(focus.counts).toEqual({ references: 0, referencedBy: 0, possible: 0 });
    expect(names(shop, focus.related)).toEqual(["coupons"]);
  });

  it("is null for a table the model lacks", () => {
    expect(focusOf(shop, 1)).toBeNull();
  });
});

describe("findMatches", () => {
  it("matches table and column names, ignoring case", () => {
    expect(names(shop, findMatches(shop, "ORDER")!)).toEqual(["order_audit", "order_events", "order_items", "orders", "payments", "shipments"]);
    expect(names(shop, findMatches(shop, " status ")!)).toEqual(["orders"]);
    expect(findMatches(shop, "zzz")!.size).toBe(0);
  });

  it("is null for a blank query", () => {
    expect(findMatches(shop, "  ")).toBeNull();
  });
});
