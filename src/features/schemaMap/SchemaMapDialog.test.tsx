// @vitest-environment jsdom
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { RelationDetail, TabId } from "../../ipc/bindings";
import { buildCatalogView, useCatalog } from "../../state/catalog";
import { dbKey } from "../../state/dbKey";
import { useLive } from "../../state/live";
import { useRelationDetail } from "../../state/relationDetail";
import { useWorkspace, type Tab } from "../../state/workspace";
import { connectedState, connectionConfig, databaseState } from "../../testing/connections";
import { cellsState } from "../../testing/notebooks";
import { shopCatalog } from "../../testing/shopCatalog";
import { SHOP_SCHEMA_MAP } from "../../testing/shopSchemaMap";
import { NotebookWorkspace } from "../editor/NotebookWorkspace";
import { dropEditor, seedEditor, textOf } from "../editor/editorStates";
import { useSchemaMapData } from "./data";
import { SchemaMapDialog } from "./SchemaMapDialog";
import { useMapViews } from "./views";

const db = SHOP_SCHEMA_MAP.db;
const ORDERS = 16448;
const ORDERS_SCRIPT = 'SELECT * FROM "public"."orders"';
const map: Tab = { id: "m", title: "Schema map", console: { db, mode: "AutoCommit" }, table: null, map: { columns: "All" } };
const sql: Tab = { id: "q", title: "Cell q", console: { db, mode: "AutoCommit" }, table: null };

const ORDERS_DETAIL: RelationDetail = {
  db,
  oid: ORDERS,
  catalogVersion: 1,
  defaults: [
    { column: "id", value: { kind: "Identity", always: false } },
    { column: "status", value: { kind: "Expr", sql: "'pending'" } },
  ],
  keys: [
    { name: "orders_pkey", columns: ["id"], kind: { kind: "Primary" } },
    { name: "orders_number_key", columns: ["number"], kind: { kind: "Unique" } },
  ],
  indexes: [
    { name: "orders_pkey", method: "btree", unique: true, primary: true, valid: true, keys: "(id)", predicate: null },
    { name: "orders_customer_idx", method: "btree", unique: false, primary: false, valid: true, keys: "(customer_id, created_at DESC)", predicate: null },
  ],
  triggers: [],
};

let detail: () => Promise<unknown>;

const dialog = () => screen.getByTestId("schema-map-dialog");
const queryDialog = () => screen.queryByTestId("schema-map-dialog");
const inDialog = () => within(dialog());
const card = (name: string) => inDialog().getByTestId(`map-table-public.${name}`);
const cellCard = (name: string) => within(screen.getByTestId("notebook-cell-m")).getByTestId(`map-table-public.${name}`);
const cells = () => useWorkspace.getState().notebooks[Object.keys(useWorkspace.getState().notebooks)[0]!]!.cells;
const click = (el: Element) => {
  fireEvent.pointerDown(el, { button: 0, clientX: 10, clientY: 10 });
  fireEvent.pointerUp(el, { clientX: 10, clientY: 10 });
};
const section = (name: string) => within(inDialog().getByRole("region", { name }));
const linkRows = (name: string) =>
  section(name)
    .queryAllByRole("button")
    .map((b) => b.textContent);
const openButton = () => within(screen.getByTestId("notebook-cell-m")).getByRole("button", { name: "Open full view" });

async function openFullView() {
  await act(async () => {
    render(
      <>
        <NotebookWorkspace />
        <SchemaMapDialog />
      </>,
    );
  });
  await screen.findByTestId("map-table-public.orders");
  await act(async () => fireEvent.click(openButton()));
  await waitFor(() => expect(card("orders")).toBeTruthy());
}

beforeEach(() => {
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  Element.prototype.scrollIntoView = () => {};
  Element.prototype.getAnimations = () => [];
  HTMLElement.prototype.setPointerCapture = () => {};
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      unobserve() {}
      disconnect() {}
    },
  );
  detail = async () => ORDERS_DETAIL;
  mockIPC((cmd) => {
    if (cmd === "schema_map") return SHOP_SCHEMA_MAP;
    if (cmd === "relation_detail") return detail();
    if (cmd === "table_script") return { text: ORDERS_SCRIPT, acceptedTexts: [ORDERS_SCRIPT] };
    return cmd === "table_order" ? { keys: [] } : null;
  });
  useWorkspace.setState({
    ...cellsState([sql, map], "q"),
    configs: { [db.connectionId]: { ...connectionConfig(db.connectionId, "prod-postgres"), environment: "Production" } },
  });
  seedEditor("q", "SELECT 1");
  useLive.setState({ connections: { [db.connectionId]: connectedState(db.connectionId, [databaseState("shop", true, ["public"])]) } });
  useCatalog.setState({ views: { [dbKey(db)]: buildCatalogView(shopCatalog(db.connectionId, 1)) } });
});

afterEach(() => {
  cleanup();
  clearMocks();
  for (const id of Object.keys(useWorkspace.getState().tabs)) dropEditor(id as TabId);
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
  useCatalog.setState(useCatalog.getInitialState(), true);
  useSchemaMapData.setState(useSchemaMapData.getInitialState(), true);
  useMapViews.setState(useMapViews.getInitialState(), true);
  useRelationDetail.setState(useRelationDetail.getInitialState(), true);
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("a map cell's full view", () => {
  it("opens from the cell's header as the active cell, named by database, schema, connection, and environment", async () => {
    await openFullView();
    expect(useWorkspace.getState().overlay).toEqual({ kind: "schemaMap", tab: "m" });
    expect(useWorkspace.getState().notebooks[Object.keys(useWorkspace.getState().notebooks)[0]!]!.active).toBe("m");
    expect(inDialog().getByRole("heading", { name: "shop · public" })).toBeTruthy();
    expect(dialog().textContent).toContain("prod-postgres");
    expect(inDialog().getByTestId("map-full-environment").textContent).toBe("Production");
    expect(dialog().querySelectorAll("[data-map-card]")).toHaveLength(15);
    expect(inDialog().getByTestId("map-size").textContent).toBe("15 tables · 12 keys");
  });

  it("uses Esc to clear the find text, then the focused table, then to close, and returns focus to Open full view", async () => {
    await openFullView();
    const find = inDialog().getByTestId("map-find") as HTMLInputElement;
    fireEvent.change(find, { target: { value: "ship" } });
    click(card("orders"));
    expect(inDialog().getByTestId("map-inspector")).toBeTruthy();

    fireEvent.keyDown(find, { key: "Escape" });
    expect([find.value, card("orders").dataset.focused, useWorkspace.getState().overlay.kind]).toEqual(["", "true", "schemaMap"]);

    fireEvent.keyDown(find, { key: "Escape" });
    expect([inDialog().queryByTestId("map-inspector"), useWorkspace.getState().overlay.kind]).toEqual([null, "schemaMap"]);

    fireEvent.keyDown(find, { key: "Escape" });
    expect(useWorkspace.getState().overlay.kind).toBe("none");
    await waitFor(() => expect(queryDialog()).toBeNull());
    expect(document.activeElement).toBe(openButton());
  });

  it("lets an open menu take the first Esc", async () => {
    await openFullView();
    const user = userEvent.setup();
    await user.click(inDialog().getByTestId("map-columns"));
    expect(await screen.findByTestId("map-columns-Keys")).toBeTruthy();
    await user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByTestId("map-columns-Keys")).toBeNull());
    expect(useWorkspace.getState().overlay.kind).toBe("schemaMap");
  });

  it("closes on a click outside it, focused table and all", async () => {
    await openFullView();
    click(card("orders"));
    const user = userEvent.setup();
    await user.click(dialog().parentElement!);
    expect(useWorkspace.getState().overlay.kind).toBe("none");
    await waitFor(() => expect(queryDialog()).toBeNull());
  });

  it("closes from its close button", async () => {
    await openFullView();
    fireEvent.click(inDialog().getByRole("button", { name: "Close full view" }));
    expect(useWorkspace.getState().overlay.kind).toBe("none");
    await waitFor(() => expect(queryDialog()).toBeNull());
    expect(document.activeElement).toBe(openButton());
  });

  it("closes with its cell", async () => {
    await openFullView();
    act(() => useWorkspace.getState().closeTab("m"));
    expect(useWorkspace.getState().overlay.kind).toBe("none");
    await waitFor(() => expect(queryDialog()).toBeNull());
  });

  it("gives way to another overlay", async () => {
    await openFullView();
    act(() => useWorkspace.getState().openQueries("history"));
    await waitFor(() => expect(queryDialog()).toBeNull());
    expect(useWorkspace.getState().overlay.kind).toBe("queries");
  });

  it("shares the focused table and the find text with the cell, but zooms on its own", async () => {
    await openFullView();
    click(card("orders"));
    fireEvent.change(inDialog().getByTestId("map-find"), { target: { value: "ship" } });
    expect(cellCard("orders").dataset.focused).toBe("true");
    expect(screen.getByTestId("map-focus-strip").textContent).toContain("orders");
    expect((within(screen.getByTestId("notebook-cell-m")).getByTestId("map-find") as HTMLInputElement).value).toBe("ship");

    fireEvent.click(inDialog().getByRole("button", { name: "Zoom in" }));
    expect(inDialog().getByTestId("map-zoom").textContent).toBe("120%");
    expect(within(screen.getByTestId("notebook-cell-m")).getByTestId("map-zoom").textContent).toBe("100%");
  });

  it("opens with the cell's focused table in the inspector", async () => {
    useMapViews.setState({ views: { m: { focus: "public.orders", find: "", moved: new Map() } } });
    await openFullView();
    expect(inDialog().getByRole("heading", { name: "orders" })).toBeTruthy();
  });
});

describe("the full view's inspector", () => {
  it("lists orders' columns, declared links both ways, and the links inferred by name", async () => {
    await openFullView();
    click(card("orders"));
    const inspector = within(inDialog().getByTestId("map-inspector"));
    expect(inspector.getByText("public · Table · ~241k rows")).toBeTruthy();
    await waitFor(() => expect(section("Columns").getByText("identity")).toBeTruthy());
    const note = (column: string) => inDialog().getByTestId("map-inspector").querySelector(`[data-column="${column}"]`)!.lastElementChild!.textContent;
    expect(["id", "number", "customer_id", "ship_address_id", "status", "total"].map(note)).toEqual([
      "identity",
      "unique",
      "→ customers",
      "→ addresses",
      "default 'pending'",
      "",
    ]);
    expect(linkRows("References")).toEqual(["customers1 colcustomer_id → id", "addresses2 cols(customer_id, ship_address_id) → (customer_id, id)"]);
    expect(linkRows("Referenced by")).toEqual(["order_items612korder_id → id", "payments238korder_id → id", "shipments120.5korder_id → id"]);
    expect(linkRows("Possible references")).toEqual(["order_audit0order_id → id", "order_events0order_id → id"]);
  });

  it("focuses the table a link row names", async () => {
    await openFullView();
    click(card("orders"));
    fireEvent.click(section("References").getByRole("button", { name: /customers/ }));
    expect([card("customers").dataset.focused, card("orders").dataset.focused]).toEqual(["true", "false"]);
  });

  it("shows the indexes and triggers once the relation's detail loads", async () => {
    let release: (v: unknown) => void = () => {};
    detail = () => new Promise((resolve) => (release = resolve));
    await openFullView();
    click(card("orders"));
    expect(section("Indexes").getByText("Loading…")).toBeTruthy();
    await act(async () => release(ORDERS_DETAIL));
    expect(
      section("Indexes")
        .getAllByRole("listitem")
        .map((li) => li.textContent),
    ).toEqual(["orders_pkeyunique(id)", "orders_customer_idxbtree(customer_id, created_at DESC)"]);
    expect(section("Triggers").getByText("None")).toBeTruthy();
  });

  it("closes the view, then browses the table's rows in a cell right after the map", async () => {
    await openFullView();
    click(card("orders"));
    await act(async () => fireEvent.click(inDialog().getByRole("button", { name: "Browse rows" })));
    expect(useWorkspace.getState().overlay.kind).toBe("none");
    await waitFor(() => expect(cells()).toHaveLength(3));
    expect(cells().indexOf("m")).toBe(1);
    expect(useWorkspace.getState().tabs[cells()[2]!]?.table?.table).toBe("orders");
  });

  it("closes the view, then queries the table in a new cell right after the map", async () => {
    await openFullView();
    click(card("orders"));
    await act(async () => fireEvent.click(inDialog().getByRole("button", { name: "New query" })));
    expect(useWorkspace.getState().overlay.kind).toBe("none");
    await waitFor(() => expect(cells()).toHaveLength(3));
    const query = cells()[2]!;
    expect([cells().indexOf("m"), useWorkspace.getState().tabs[query]?.table, textOf(query)]).toEqual([1, null, ORDERS_SCRIPT]);
  });
});
