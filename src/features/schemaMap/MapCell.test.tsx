// @vitest-environment jsdom
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { TabId } from "../../ipc/bindings";
import { buildCatalogView, useCatalog } from "../../state/catalog";
import { dbKey } from "../../state/dbKey";
import { useLive } from "../../state/live";
import { useWorkspace, type Tab } from "../../state/workspace";
import { connectedState, connectionConfig, databaseState } from "../../testing/connections";
import { bindToFile, cellsState } from "../../testing/notebooks";
import { shopCatalog } from "../../testing/shopCatalog";
import { SHOP_SCHEMA_MAP } from "../../testing/shopSchemaMap";
import { NotebookWorkspace } from "../editor/NotebookWorkspace";
import { dropEditor, seedEditor, textOf } from "../editor/editorStates";
import { notebookDirty } from "../notebooks/notebookDirty";
import { useSchemaMapData } from "./data";
import { useMapViews } from "./views";

const db = SHOP_SCHEMA_MAP.db;
const ORDERS_SCRIPT = 'SELECT * FROM "public"."orders"';
const map: Tab = { id: "m", title: "Schema map", console: { db, mode: "AutoCommit" }, table: null, map: { columns: "All" } };
const sql: Tab = { id: "q", title: "Cell q", console: { db, mode: "AutoCommit" }, table: null };

let reply: () => Promise<unknown>;

const card = (name: string) => screen.getByTestId(`map-table-public.${name}`);
const canvas = () => screen.getByTestId("map-cell-m");
const cells = () => useWorkspace.getState().notebooks[Object.keys(useWorkspace.getState().notebooks)[0]!]!.cells;
const click = (el: Element) => {
  fireEvent.pointerDown(el, { button: 0, clientX: 10, clientY: 10 });
  fireEvent.pointerUp(el, { clientX: 10, clientY: 10 });
};
const rows = (name: string) => card(name).querySelectorAll("[data-column]").length;

async function renderMap() {
  await act(async () => {
    render(<NotebookWorkspace />);
  });
  await screen.findByTestId("map-table-public.orders");
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
  reply = async () => SHOP_SCHEMA_MAP;
  mockIPC((cmd) => {
    if (cmd === "schema_map") return reply();
    if (cmd === "table_script") return { text: ORDERS_SCRIPT, acceptedTexts: [ORDERS_SCRIPT] };
    if (cmd === "catalog_refresh") return 2;
    return cmd === "table_order" ? { keys: [] } : null;
  });
  useWorkspace.setState({ ...cellsState([sql, map], "m"), configs: { [db.connectionId]: connectionConfig(db.connectionId, "shop") } });
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
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("a schema map cell", () => {
  it("draws every table of the schema, titled by database and schema, with the legend's counts", async () => {
    await renderMap();
    const cell = screen.getByTestId("notebook-cell-m");
    expect(within(cell).getByTestId("tab-m").textContent).toBe("shop · publicSchema map");
    expect(cell.querySelectorAll("[data-map-card]")).toHaveLength(15);
    expect(screen.getByTestId("map-size").textContent).toBe("15 tables · 12 keys");
    expect(screen.queryByTestId("map-partial")).toBeNull();
    expect(
      within(cell)
        .getAllByRole("button")
        .slice(0, 5)
        .map((b) => b.getAttribute("aria-label") ?? b.textContent),
    ).toEqual(["Collapse cell", "shop · publicSchema map", "Refresh catalog", "Open full view", "Close cell"]);
  });

  it("focuses orders on a click, with its links counted in the strip, and clears it on Esc or a background click", async () => {
    await renderMap();
    click(card("orders"));
    expect(screen.queryByTestId("map-legend")).toBeNull();
    const strip = screen.getByTestId("map-focus-strip");
    expect(strip.textContent).toContain("orders");
    expect(strip.textContent).toContain("9 columns · ~241k rows");
    expect(screen.getByTestId("map-focus-counts").textContent).toBe("References 2·Referenced by 3·Possible 2");
    expect(screen.getByTestId("map-focus-pill").textContent).toContain("5 keys · 2 inferred");
    expect([card("orders").dataset.focused, card("customers").dataset.lit, card("suppliers").dataset.lit]).toEqual(["true", "true", "false"]);

    fireEvent.keyDown(canvas(), { key: "Escape" });
    expect([screen.queryByTestId("map-focus-strip"), screen.getByTestId("map-legend")]).toEqual([null, expect.anything()]);

    click(card("orders"));
    click(canvas());
    expect(screen.queryByTestId("map-focus-strip")).toBeNull();
  });

  it("lets Esc through when nothing is focused", async () => {
    await renderMap();
    const event = new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true });
    canvas().dispatchEvent(event);
    expect(event.defaultPrevented).toBe(false);
  });

  it("shows only key columns in Keys, and the change makes a file notebook unsaved", async () => {
    await renderMap();
    const notebook = Object.keys(useWorkspace.getState().notebooks)[0]!;
    bindToFile(notebook);
    expect(notebookDirty(notebook)).toBe(false);
    const allRows = rows("orders");
    const user = userEvent.setup();
    await user.click(screen.getByTestId("map-columns"));
    await user.click(await screen.findByTestId("map-columns-Keys"));
    expect(useWorkspace.getState().tabs.m?.map).toEqual({ columns: "Keys" });
    expect(screen.getByTestId("map-columns").textContent).toContain("Keys");
    expect([allRows, rows("orders")]).toEqual([9, 3]);
    expect(notebookDirty(notebook)).toBe(true);
  });

  it("lights the tables Find matches and marks the matched text", async () => {
    await renderMap();
    fireEvent.change(screen.getByTestId("map-find"), { target: { value: "ship" } });
    expect([card("shipments").dataset.lit, card("orders").dataset.lit, card("customers").dataset.lit]).toEqual(["true", "true", "false"]);
    expect([...card("orders").querySelectorAll("mark")].map((m) => m.textContent)).toEqual(["ship"]);
    fireEvent.keyDown(screen.getByTestId("map-find"), { key: "Escape" });
    expect([(screen.getByTestId("map-find") as HTMLInputElement).value, card("customers").dataset.lit]).toEqual(["", "true"]);
  });

  it("browses the focused table's rows, and queries it in a new cell, both right after the map", async () => {
    await renderMap();
    click(card("orders"));
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Browse rows" })));
    await waitFor(() => expect(cells()).toHaveLength(3));
    const browse = cells()[2]!;
    expect(useWorkspace.getState().tabs[browse]?.table?.table).toBe("orders");

    act(() => useWorkspace.getState().setActive("m"));
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Query in new cell" })));
    await waitFor(() => expect(cells()).toHaveLength(4));
    const query = cells()[2]!;
    expect([useWorkspace.getState().tabs[query]?.table, textOf(query)]).toEqual([null, ORDERS_SCRIPT]);
  });

  it("drops its focus and find when the cell closes", async () => {
    await renderMap();
    click(card("orders"));
    expect(useMapViews.getState().views.m?.focus).toBe("public.orders");
    fireEvent.click(within(screen.getByTestId("notebook-cell-m")).getByRole("button", { name: "Close cell" }));
    await waitFor(() => expect(useWorkspace.getState().tabs.m).toBeUndefined());
    expect(useMapViews.getState().views.m).toBeUndefined();
  });
});

describe("a schema map cell's states", () => {
  it("shows a spinner on the first load, then the map", async () => {
    let release: (v: unknown) => void = () => {};
    reply = () => new Promise((resolve) => (release = resolve));
    await act(async () => {
      render(<NotebookWorkspace />);
    });
    expect(screen.getByRole("status").textContent).toBe(" Loading the schema map…");
    await act(async () => release(SHOP_SCHEMA_MAP));
    expect(screen.queryByRole("status")).toBeNull();
    expect(card("orders")).toBeTruthy();
  });

  it("names a failed load and retries it", async () => {
    reply = async () => {
      throw { kind: "Failed", message: "permission denied for schema public" };
    };
    await act(async () => {
      render(<NotebookWorkspace />);
    });
    expect(screen.getByRole("status").textContent).toContain("permission denied for schema public");
    reply = async () => SHOP_SCHEMA_MAP;
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Retry" })));
    expect(await screen.findByTestId("map-table-public.orders")).toBeTruthy();
  });

  it("asks for a connection while no catalog is served", async () => {
    useLive.setState({ connections: {} });
    await act(async () => {
      render(<NotebookWorkspace />);
    });
    expect(screen.getByRole("status").textContent).toBe("Connect to shop to draw this schema's map.");
  });

  it("warns that a truncated map is partial, also while a table is focused", async () => {
    reply = async () => ({ ...SHOP_SCHEMA_MAP, omittedTables: 1_120, keysTruncated: true, partitionsTruncated: true });
    await renderMap();
    const warning = "Partial map: 1,120 tables and some keys left out (limit 500 tables, 2,000 keys); partitions past 10,000 drawn as tables";
    expect(within(screen.getByTestId("map-legend")).getByTestId("map-partial").textContent).toBe(warning);
    click(card("orders"));
    const strip = within(screen.getByTestId("map-focus-strip")).getByTestId("map-partial");
    expect([strip.textContent, strip.title]).toEqual(["Partial map", warning]);
  });

  it("says when the schema has no tables", async () => {
    reply = async () => ({ ...SHOP_SCHEMA_MAP, tables: [], links: [] });
    await act(async () => {
      render(<NotebookWorkspace />);
    });
    expect(screen.getByRole("status").textContent).toBe("No tables in public.");
  });
});
