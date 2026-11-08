// @vitest-environment jsdom
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, expect, it, vi } from "vitest";
import {
  commands,
  type CellAggregate,
  type ColumnMeta,
  type ColumnProfile,
  type DatabaseKind,
  type RelationDetail,
  type ResultMeta,
  type RowPage,
  type TableScript,
  type TypeCategory,
} from "../../ipc/bindings";
import { buildCatalogView, useCatalog } from "../../state/catalog";
import { dbKey } from "../../state/dbKey";
import { useRelationDetail } from "../../state/relationDetail";
import { dropRowCaches, PAGE_SIZE, rowCacheFor } from "../../state/rowCache";
import { markDeleted, NO_EDITS, setCell, useTableEdits } from "../../state/tableEdits";
import { useWorkspace } from "../../state/workspace";
import { Overlays } from "../../shell/Overlays";
import { shopCatalog } from "../../testing/shopCatalog";
import { dropEditor } from "../editor/editorStates";
import { gridEditing, gridKeyColumns, HeaderIcon, nextFocus, primaryKeyColumns, ResultGrid, selectRows } from "./ResultGrid";
import { acceptSelectionTotals, type SelectionSlot } from "./selectionTotals";

const iconOf = (category: TypeCategory, primaryKey = false) => {
  const html = renderToStaticMarkup(<HeaderIcon column={{ name: "c", type: { display: "t", category }, source: null }} primaryKey={primaryKey} />);
  return html.match(/lucide-([a-z-]+)/)?.[1];
};

it("marks each column with an icon for its type", () => {
  const categories: TypeCategory[] = [
    "Integer",
    "Numeric",
    "Text",
    "Bool",
    "Temporal",
    "Json",
    "Uuid",
    "Binary",
    "Network",
    "Array",
    "Enum",
    "Composite",
    "Range",
    "Other",
  ];
  expect(categories.map((c) => iconOf(c))).toEqual([
    "hash",
    "hash",
    "case-sensitive",
    "toggle-left",
    "calendar",
    "braces",
    "fingerprint-pattern",
    "binary",
    "network",
    "brackets",
    "list",
    "box",
    "move-horizontal",
    "circle-dot",
  ]);
});

it("shows the key instead of the type on a primary key column", () => {
  expect(iconOf("Integer", true)).toBe("key-round");
});

it("looks primary keys up in the catalog of the database the rows came from", () => {
  const shop = buildCatalogView(shopCatalog("conn", 1));
  const orders = shop.catalog.schemas[1]!.relations.find((r) => r.name === "orders")!;
  // In `reporting`, the OID `orders` has in `shop` belongs to a table whose key is its second column.
  const reporting = buildCatalogView({
    databaseKind: "Postgres",
    db: { connectionId: "conn", database: "reporting" },
    version: 2,
    defaultSchema: "public",
    schemas: [
      {
        name: "public",
        quoted: "public",
        system: false,
        tableCount: 1,
        relations: [
          {
            ...orders,
            name: "daily",
            columns: [
              { ...orders.columns[0]!, name: "orders", pk: false },
              { ...orders.columns[1]!, name: "day", pk: true },
            ],
          },
        ],
        functions: [],
        sequences: [],
        types: [],
        synonyms: [],
      },
    ],
    extensions: [],
  });
  const views = { [dbKey(shop.catalog.db)]: shop, [dbKey(reporting.catalog.db)]: reporting };
  const column = (name: string, attnum: number): ColumnMeta => ({
    name,
    type: { display: "bigint", category: "Numeric" },
    source: { tableOid: orders.oid, attnum },
  });
  const columns = [column("a", orders.columns[0]!.attnum), column("b", orders.columns[1]!.attnum)];
  expect(primaryKeyColumns(views, { db: { connectionId: "conn", database: "shop" }, columns })).toEqual([true, false]);
  expect(primaryKeyColumns(views, { db: { connectionId: "conn", database: "reporting" }, columns })).toEqual([false, true]);
  expect(primaryKeyColumns(views, { db: { connectionId: "conn", database: "postgres" }, columns })).toEqual([false, false]);
});

it("moves the active cell by key, clamped to the result", () => {
  const from = { row: 5, col: 2 };
  const at = (key: string, mod = false) => nextFocus(key, mod, from, 100, 8);
  expect(at("ArrowDown")).toEqual({ row: 6, col: 2 });
  expect(at("ArrowLeft")).toEqual({ row: 5, col: 1 });
  expect(at("PageUp")).toEqual({ row: 0, col: 2 });
  expect(at("PageDown")).toEqual({ row: 25, col: 2 });
  expect(at("Tab")).toBeNull();
});

it("jumps to the edges with Home, End, and the modifier", () => {
  const from = { row: 5, col: 2 };
  const at = (key: string, mod = false) => nextFocus(key, mod, from, 100, 8);
  expect(at("Home")).toEqual({ row: 5, col: 0 });
  expect(at("End")).toEqual({ row: 5, col: 7 });
  expect(at("Home", true)).toEqual({ row: 0, col: 0 });
  expect(at("End", true)).toEqual({ row: 99, col: 7 });
  expect(at("ArrowDown", true)).toEqual({ row: 99, col: 2 });
  expect(at("ArrowUp", true)).toEqual({ row: 0, col: 2 });
  expect(at("ArrowRight", true)).toEqual({ row: 5, col: 7 });
  expect(at("ArrowLeft", true)).toEqual({ row: 5, col: 0 });
});

it("selects whole rows from the first to the last column, focused on the first", () => {
  expect(selectRows(4, 4, 6)).toEqual({ anchor: { row: 4, col: 5 }, focus: { row: 4, col: 0 } });
  expect(selectRows(2, 7, 6)).toEqual({ anchor: { row: 2, col: 5 }, focus: { row: 7, col: 0 } });
});

it("keeps generated columns read-only and requires database-scoped edit provenance", () => {
  const catalog = shopCatalog("ms", 1);
  const relation = catalog.schemas[1]!.relations.find((r) => r.name === "orders")!;
  relation.columns[0]!.writable = false;
  const meta = {
    databaseKind: catalog.databaseKind,
    db: catalog.db,
    columns: relation.columns.map((column) => ({
      name: column.name,
      type: { display: column.typeDisplay, category: column.category },
      source: { tableOid: relation.oid, attnum: column.attnum },
    })),
  };
  const view = buildCatalogView(catalog);
  const editing = gridEditing(view, "public", "orders", meta);
  expect(editing?.pkColumns).toEqual(["id"]);
  expect(editing?.writableColumns.has("id")).toBe(false);
  expect(editing?.writableColumns.has("status")).toBe(true);
  expect(gridEditing(view, "public", "orders", { ...meta, db: { ...meta.db, database: "reporting" } })).toBeNull();
  expect(gridEditing(view, "public", "orders", { ...meta, columns: meta.columns.map((c) => ({ ...c, source: null })) })).toBeNull();
});

it.each<{ catalogKind: DatabaseKind; resultKind: DatabaseKind }>([
  { catalogKind: "SqlServer", resultKind: "SqlServer" },
  { catalogKind: "SqlServer", resultKind: "Postgres" },
  { catalogKind: "Postgres", resultKind: "SqlServer" },
])("keeps a $resultKind result read-only with a $catalogKind catalog despite valid key provenance", ({ catalogKind, resultKind }) => {
  const catalog = shopCatalog("ms", 1);
  const relation = catalog.schemas[1]!.relations.find((r) => r.name === "orders")!;
  const meta = {
    databaseKind: catalog.databaseKind,
    db: catalog.db,
    columns: relation.columns.map((column) => ({
      name: column.name,
      type: { display: column.typeDisplay, category: column.category },
      source: { tableOid: relation.oid, attnum: column.attnum },
    })),
  };
  expect(gridEditing(buildCatalogView(catalog), "public", "orders", meta)?.pkColumns).toEqual(["id"]);
  expect(gridEditing(buildCatalogView({ ...catalog, databaseKind: catalogKind }), "public", "orders", { ...meta, databaseKind: resultKind })).toBeNull();
});

it.each(["update", "delete"])("keeps original key columns for a pending %s when the key changes or editing is refused", (change) => {
  const selected = [{ column: "id", value: "1" }];
  const edits = change === "update" ? setCell(NO_EDITS, selected, "note", "edited", "original") : markDeleted(NO_EDITS, [selected], true);
  expect(gridKeyColumns({ pkColumns: ["alternate"], writableColumns: new Set(["note"]) }, edits)).toEqual(["id"]);
  expect(gridKeyColumns(null, edits)).toEqual(["id"]);
});

const shopDb = { connectionId: "c1", database: "shop" };
const foreignKey = (name: string, columns: string[], refTable: string, refColumns: string[]) => ({
  name,
  columns,
  kind: { kind: "Foreign" as const, refSchema: "public", refTable, refColumns, onDelete: "NoAction" as const, onUpdate: "NoAction" as const },
});
const ordersDetail = (oid: number, extraKeys: RelationDetail["keys"]): RelationDetail => ({
  db: shopDb,
  oid,
  catalogVersion: 1,
  defaults: [],
  keys: [
    { name: "orders_pkey", columns: ["id"], kind: { kind: "Primary" } },
    foreignKey("orders_customer_id_fkey", ["customer_id"], "customers", ["id"]),
    ...extraKeys,
  ],
  indexes: [],
  triggers: [],
});

type ResultSpec = {
  columns?: { name: string; attnum: number | null }[];
  databaseKind?: DatabaseKind;
  allRows?: ResultMeta["allRows"];
  cut?: [number, number][];
  cell?: (row: number, col: number) => string;
  keys?: RelationDetail["keys"];
};

const ORDER_COLUMNS = [
  { name: "id", attnum: 1 },
  { name: "customer_id", attnum: 2 },
];

let detailReads = 0;

function queryResult(rows: (string | null)[][], spec: ResultSpec = {}): ResultMeta {
  const catalog = shopCatalog("c1", 1);
  const orders = catalog.schemas[1]!.relations.find((r) => r.name === "orders")!;
  orders.columns[1]!.fk = true;
  useCatalog.setState({ views: { [dbKey(shopDb)]: buildCatalogView(catalog) } });
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockReturnValue(800);
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockReturnValue(600);
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      unobserve() {}
      disconnect() {}
    },
  );
  mockIPC((cmd, args): RowPage | RelationDetail | TableScript | CellAggregate | string | null => {
    if (cmd === "result_page") return { start: 0, rows, cut: spec.cut ?? [] };
    if (cmd === "result_aggregate") return { kind: "Counts", count: 0, nonNull: 0 };
    if (cmd === "result_cell") {
      const a = args as { row: number; col: number };
      return spec.cell!(a.row, a.col);
    }
    if (cmd === "relation_detail") {
      detailReads++;
      return ordersDetail(orders.oid, spec.keys ?? []);
    }
    if (cmd === "table_script") return { text: "SELECT * FROM public.customers", acceptedTexts: ["SELECT * FROM public.customers"] };
    return null;
  });
  return {
    databaseKind: spec.databaseKind ?? "Postgres",
    ref: { tabId: "q1", runId: 1, index: 0 },
    db: shopDb,
    columns: (spec.columns ?? ORDER_COLUMNS).map(({ name, attnum }) => ({
      name,
      type: { display: name === "status" ? "text" : "bigint", category: name === "status" ? "Text" : "Integer" },
      source: attnum === null ? null : { tableOid: orders.oid, attnum },
    })),
    rowCount: rows.length,
    rowsSeen: rows.length,
    truncated: null,
    elapsedMs: 1,
    allRows: spec.allRows ?? { kind: "Statement" },
  };
}

afterEach(() => {
  cleanup();
  clearMocks();
  detailReads = 0;
  dropRowCaches("q1");
  for (const id of Object.keys(useWorkspace.getState().tabs)) dropEditor(id);
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useCatalog.setState(useCatalog.getInitialState(), true);
  useRelationDetail.setState(useRelationDetail.getInitialState(), true);
  useTableEdits.setState(useTableEdits.getInitialState(), true);
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

it("follows a foreign key from a query result to the referenced row, and not from a NULL", async () => {
  const user = userEvent.setup();
  render(
    <ResultGrid
      meta={queryResult([
        ["1", "42"],
        ["2", null],
      ])}
      done
      query={null}
      editing={null}
    />,
  );
  await user.pointer({ keys: "[MouseRight]", target: await screen.findByText("NULL") });
  expect((await screen.findByTestId("grid-menu-follow-orders_customer_id_fkey")).getAttribute("aria-disabled")).toBe("true");
  await user.keyboard("{Escape}");

  await user.pointer({ keys: "[MouseRight]", target: screen.getByText("42") });
  const follow = await screen.findByTestId("grid-menu-follow-orders_customer_id_fkey");
  expect([follow.textContent, follow.hasAttribute("aria-disabled"), screen.queryByTestId("grid-menu-filter-eq")]).toEqual([
    "Go to referenced row in customers",
    false,
    null,
  ]);
  await user.click(follow);
  await vi.waitFor(() =>
    expect(Object.values(useWorkspace.getState().tabs).map((t) => [t.console.db, t.table])).toEqual([
      [shopDb, { schema: "public", table: "customers", page: 0, query: { orderBy: "", where: "", filters: [{ column: "id", op: "Eq", value: "42" }] } }],
    ]),
  );
});

it("opens the focused row as a record with ⌘I or the menu", async () => {
  const user = userEvent.setup();
  const meta = queryResult([
    ["1", "42"],
    ["2", null],
  ]);
  render(<ResultGrid meta={meta} done query={null} editing={null} />);
  await user.click(await screen.findByText("2"));
  await user.keyboard("{Meta>}i{/Meta}");
  expect(useWorkspace.getState().overlay).toEqual({ kind: "record", meta, row: 1 });

  useWorkspace.getState().close();
  await user.pointer({ keys: "[MouseRight]", target: screen.getByText("42") });
  const item = await screen.findByRole("menuitem", { name: "View as record" });
  expect(item.getAttribute("aria-keyshortcuts")).toBe("Meta+I");
  await user.click(item);
  expect(useWorkspace.getState().overlay).toEqual({ kind: "record", meta, row: 0 });
});

const EDITING = { pkColumns: ["id"], writableColumns: new Set(["customer_id"]) };
const lastOpened = (table: string) =>
  Object.values(useWorkspace.getState().tabs)
    .filter((t) => t.table?.table === table)
    .at(-1)?.table?.query.filters;
const byId = (value: string) => [{ column: "id", op: "Eq", value }];

async function followFrom(user: ReturnType<typeof userEvent.setup>, cell: HTMLElement, key = "orders_customer_id_fkey") {
  await user.pointer({ keys: "[MouseRight]", target: cell });
  await user.click(await screen.findByTestId(`grid-menu-follow-${key}`));
}

it("follows a key from a SQL Server table page, whose columns carry their table column", async () => {
  const user = userEvent.setup();
  render(<ResultGrid meta={queryResult([["1", "42"]], { databaseKind: "SqlServer", allRows: { kind: "Table" } })} done query={null} editing={null} />);
  await followFrom(user, await screen.findByText("42"));
  await vi.waitFor(() => expect(lastOpened("customers")).toEqual(byId("42")));
});

it("offers no follow and reads no keys for columns without a table column, as a SQL Server statement result has", async () => {
  const user = userEvent.setup();
  const meta = queryResult([["1", "42"]], { databaseKind: "SqlServer", columns: ORDER_COLUMNS.map((c) => ({ ...c, attnum: null })) });
  render(<ResultGrid meta={meta} done query={null} editing={null} />);
  await user.pointer({ keys: "[MouseRight]", target: await screen.findByText("42") });
  await screen.findByTestId("grid-menu-record");
  expect([screen.queryByTestId("grid-menu-follow-orders_customer_id_fkey"), detailReads]).toEqual([null, 0]);
});

it("disables View as record on a staged new row", async () => {
  const user = userEvent.setup();
  const meta = queryResult([["1", "42"]]);
  useTableEdits.getState().addRow("q1");
  render(<ResultGrid meta={meta} done query={null} editing={EDITING} />);
  await screen.findByText("42");
  await user.pointer({ keys: "[MouseRight]", target: screen.getByTestId("grid-cell-1-1") });
  expect((await screen.findByTestId("grid-menu-record")).getAttribute("aria-disabled")).toBe("true");
  await user.keyboard("{Escape}");
  await user.pointer({ keys: "[MouseRight]", target: screen.getByText("42") });
  expect((await screen.findByTestId("grid-menu-record")).hasAttribute("aria-disabled")).toBe(false);
});

it("follows a cut key by its full value, and a staged key by the value the grid shows", async () => {
  const user = userEvent.setup();
  const meta = queryResult(
    [
      ["1", "4200"],
      ["2", "4300"],
    ],
    {
      cut: [
        [0, 1],
        [1, 1],
      ],
      cell: (row) => (row === 0 ? "42000000" : "43000000"),
    },
  );
  useTableEdits.getState().edit("q1", (e) => setCell(e, [{ column: "id", value: "2" }], "customer_id", "7", "4300"));
  render(<ResultGrid meta={meta} done query={null} editing={EDITING} />);
  await followFrom(user, await screen.findByTestId("grid-cell-0-1"));
  await vi.waitFor(() => expect(lastOpened("customers")).toEqual(byId("42000000")));
  await followFrom(user, screen.getByText("7"));
  await vi.waitFor(() => expect(lastOpened("customers")).toEqual(byId("7")));
});

it("offers each copy of a single-column key its own follow", async () => {
  const user = userEvent.setup();
  const columns = [...ORDER_COLUMNS, ...ORDER_COLUMNS];
  render(<ResultGrid meta={queryResult([["1", "42", "2", "9"]], { columns })} done query={null} editing={null} />);
  await followFrom(user, await screen.findByText("9"));
  await vi.waitFor(() => expect(lastOpened("customers")).toEqual(byId("9")));
});

it("names the key columns when two keys from one cell reach the same table", async () => {
  const user = userEvent.setup();
  const meta = queryResult([["1", "42", "shipped"]], {
    columns: [...ORDER_COLUMNS, { name: "status", attnum: 3 }],
    keys: [foreignKey("orders_customer_id_status_fkey", ["customer_id", "status"], "customers", ["id", "region"])],
  });
  render(<ResultGrid meta={meta} done query={null} editing={null} />);
  await user.pointer({ keys: "[MouseRight]", target: await screen.findByText("42") });
  await screen.findByTestId("grid-menu-follow-orders_customer_id_status_fkey");
  expect(
    screen
      .getAllByRole("menuitem")
      .map((item) => item.textContent)
      .filter((label) => label?.startsWith("Go to")),
  ).toEqual(["Go to referenced row in customers (customer_id)", "Go to referenced row in customers (customer_id, status)"]);
});

it.each(["{Shift>}{F10}{/Shift}", "{ContextMenu}"])("opens the focused cell's menu from the keyboard with %s", async (keys) => {
  const user = userEvent.setup();
  render(<ResultGrid meta={queryResult([["1", "42"]])} done query={null} editing={null} />);
  await user.click(await screen.findByText("1"));
  await user.keyboard("{ArrowRight}");
  await user.keyboard(keys);
  await user.click(await screen.findByTestId("grid-menu-follow-orders_customer_id_fkey"));
  await vi.waitFor(() => expect(lastOpened("customers")).toEqual(byId("42")));
});

it("returns focus to the grid when a record view opened from the menu closes", async () => {
  const user = userEvent.setup();
  render(
    <>
      <ResultGrid meta={queryResult([["1", "42"]])} done query={null} editing={null} />
      <Overlays />
    </>,
  );
  await user.pointer({ keys: "[MouseRight]", target: await screen.findByText("42") });
  await user.click(await screen.findByTestId("grid-menu-record"));
  await screen.findByTestId("record-dialog");
  await user.keyboard("{Escape}");
  await vi.waitFor(() => expect(document.activeElement).toBe(screen.getByTestId("result-grid")));
});

it("loads its visible rows again after the record view pages them out of the shared cache", async () => {
  const meta = { ...queryResult([]), rowCount: 20_000, rowsSeen: 20_000 };
  mockIPC((cmd, args) => {
    if (cmd !== "result_page") return null;
    const { start, len } = args as { start: number; len: number };
    return { start, rows: Array.from({ length: len }, (_, i) => [String(start + i + 1), "c"]), cut: [] };
  });
  render(<ResultGrid meta={meta} done query={null} editing={null} />);
  await vi.waitFor(() => expect(screen.getByTestId("grid-cell-0-0").textContent).toBe("1"));
  const cache = rowCacheFor(meta.ref);
  for (let page = 10; page < 80; page++) await cache.ensure({ start: page * PAGE_SIZE, end: page * PAGE_SIZE + 1 }, meta.rowCount);
  await vi.waitFor(() => expect(screen.getByTestId("grid-cell-0-0").textContent).toBe("1"));
});

function SelectionHarness({ meta, editing }: { meta: ResultMeta; editing: { pkColumns: string[]; writableColumns: ReadonlySet<string> } | null }) {
  const shownKey = `${meta.ref.tabId}:${meta.ref.runId}:${meta.ref.index}`;
  const [slot, setSlot] = useState<SelectionSlot | null>(null);
  const text = slot?.key === shownKey ? slot.text : null;
  return (
    <>
      {text !== null && (
        <span data-testid="result-selection" title={text}>
          {text}
        </span>
      )}
      <ResultGrid
        meta={meta}
        done
        query={null}
        editing={editing}
        onSelectionTotals={(next) => setSlot((current) => acceptSelectionTotals(current, next, shownKey))}
      />
    </>
  );
}

function numericTotal(sum: string, avg = "0.5", min = "0.25", max = "0.75"): { status: "ok"; data: CellAggregate } {
  return { status: "ok", data: { kind: "Numeric", count: 2, nonNull: 2, sum, avg, min, max } };
}

it("leaves the selection total hidden for one cell and does not ask for a total", async () => {
  const user = userEvent.setup();
  const aggregate = vi.spyOn(commands, "resultAggregate").mockResolvedValue(numericTotal("999"));
  render(<SelectionHarness meta={queryResult([["1", "10"]])} editing={null} />);
  await user.click(await screen.findByTestId("grid-cell-0-0"));
  await vi.waitFor(() => expect(screen.getByTestId("grid-cell-0-0").getAttribute("aria-selected")).toBe("true"));
  expect(screen.queryByTestId("result-selection")).toBeNull();
  expect(aggregate).not.toHaveBeenCalled();
});

it("shows the mocked sum for two cells instead of adding the painted text", async () => {
  const user = userEvent.setup();
  vi.spyOn(commands, "resultAggregate").mockResolvedValue(numericTotal("1000", "2000.50", "3000", "4000"));
  render(
    <SelectionHarness
      meta={queryResult(
        [
          ["1", "10"],
          ["2", "20"],
        ],
        { columns: [{ name: "total", attnum: 4 }] },
      )}
      editing={null}
    />,
  );
  await user.click(await screen.findByTestId("grid-cell-0-0"));
  await user.keyboard("{Shift>}{ArrowDown}{/Shift}");
  const span = await screen.findByTestId("result-selection");
  expect(span.textContent).toBe("2 cells · 2 non-null · Sum 1000 · Avg 2000.50 · Min 3000 · Max 4000");
});

it("counts a selection across an integer column and a text column without a sum", async () => {
  const user = userEvent.setup();
  vi.spyOn(commands, "resultAggregate").mockResolvedValue({ status: "ok", data: { kind: "Counts", count: 4, nonNull: 4 } });
  render(
    <SelectionHarness
      meta={queryResult(
        [
          ["1", "paid"],
          ["2", "open"],
        ],
        {
          columns: [
            { name: "id", attnum: 1 },
            { name: "status", attnum: 3 },
          ],
        },
      )}
      editing={null}
    />,
  );
  await user.click(await screen.findByTestId("result-grid"));
  await user.keyboard("{Meta>}a{/Meta}");
  const span = await screen.findByTestId("result-selection");
  expect(span.textContent).toContain("4 cells");
  expect(span.textContent).toContain("4 non-null");
  expect(span.textContent).not.toContain("Sum");
});

it("keeps a staged value on screen and says the stored sum excludes it", async () => {
  const user = userEvent.setup();
  vi.spyOn(commands, "resultAggregate").mockResolvedValue(numericTotal("50.00", "25.00", "10", "20"));
  const meta = queryResult([
    ["1", "10"],
    ["2", "20"],
  ]);
  render(<SelectionHarness meta={meta} editing={EDITING} />);
  await screen.findByText("10");
  useTableEdits.getState().edit("q1", (e) => setCell(e, [{ column: "id", value: "1" }], "customer_id", "77", "10"));
  await user.click(await screen.findByText("77"));
  await user.keyboard("{Shift>}{ArrowDown}{/Shift}");
  const span = await screen.findByTestId("result-selection");
  expect(span.textContent).toContain("excludes pending edits");
  expect(span.textContent).toContain("Sum 50.00");
  expect(screen.getByTestId("grid-cell-0-1").textContent).toContain("77");
});

it("names pending edits and does not total a selection of only new rows", async () => {
  const user = userEvent.setup();
  const aggregate = vi.spyOn(commands, "resultAggregate").mockResolvedValue(numericTotal("999"));
  const meta = queryResult([]);
  useTableEdits.getState().addRow("q1");
  useTableEdits.getState().addRow("q1");
  render(<SelectionHarness meta={meta} editing={EDITING} />);
  await user.click(await screen.findByTestId("grid-cell-0-0"));
  await user.keyboard("{Shift>}{ArrowDown}{/Shift}");
  expect((await screen.findByTestId("result-selection")).textContent).toBe("excludes pending edits");
  expect(aggregate).not.toHaveBeenCalled();
});

it("shows the later rectangle's sum when that reply arrives first", async () => {
  const user = userEvent.setup();
  const resolvers: ((value: { status: "ok"; data: CellAggregate }) => void)[] = [];
  vi.spyOn(commands, "resultAggregate").mockImplementation(
    () =>
      new Promise((resolve) => {
        resolvers.push(resolve);
      }),
  );
  render(<SelectionHarness meta={queryResult([["4"], ["5"], ["6"]], { columns: [{ name: "total", attnum: 4 }] })} editing={null} />);
  await user.click(await screen.findByTestId("grid-cell-0-0"));
  await user.keyboard("{Shift>}{ArrowDown}{/Shift}");
  await vi.waitFor(() => expect(resolvers).toHaveLength(1));
  await user.click(screen.getByTestId("grid-cell-1-0"));
  await user.keyboard("{Shift>}{ArrowDown}{/Shift}");
  await vi.waitFor(() => expect(resolvers).toHaveLength(2));
  expect(screen.getByTestId("grid-cell-2-0").getAttribute("aria-selected")).toBe("true");
  expect(screen.getByTestId("grid-cell-0-0").getAttribute("aria-selected")).toBe("false");
  resolvers[1]!({ status: "ok", data: { kind: "Numeric", count: 2, nonNull: 2, sum: "later", avg: "0", min: "0", max: "0" } });
  await vi.waitFor(() => expect(screen.getByTestId("result-selection").textContent).toContain("Sum later"));
  resolvers[0]!({ status: "ok", data: { kind: "Numeric", count: 2, nonNull: 2, sum: "earlier", avg: "0", min: "0", max: "0" } });
  await vi.waitFor(() => expect(screen.getByTestId("result-selection").textContent).toContain("Sum later"));
  expect(screen.getByTestId("result-selection").textContent).not.toContain("earlier");
});

it("publishes the header once when a drag ends, not on each cell entered", async () => {
  const resolvers: ((value: { status: "ok"; data: CellAggregate }) => void)[] = [];
  vi.spyOn(commands, "resultAggregate").mockImplementation(
    () =>
      new Promise((resolve) => {
        resolvers.push(resolve);
      }),
  );
  const meta = queryResult([["1"], ["2"], ["3"]], { columns: [{ name: "total", attnum: 4 }] });
  const shownKey = `${meta.ref.tabId}:${meta.ref.runId}:${meta.ref.index}`;
  let publishes = 0;
  function Counting() {
    const [slot, setSlot] = useState<SelectionSlot | null>(null);
    const text = slot?.key === shownKey ? slot.text : null;
    return (
      <>
        {text !== null && <span data-testid="result-selection">{text}</span>}
        <ResultGrid
          meta={meta}
          done
          query={null}
          editing={null}
          onSelectionTotals={(next) => {
            publishes += 1;
            setSlot((current) => acceptSelectionTotals(current, next, shownKey));
          }}
        />
      </>
    );
  }
  render(<Counting />);
  await screen.findByTestId("grid-cell-0-0");
  await vi.waitFor(() => expect(publishes).toBeGreaterThan(0));
  const before = publishes;
  fireEvent.mouseDown(screen.getByTestId("grid-cell-0-0"), { button: 0 });
  fireEvent.mouseEnter(screen.getByTestId("grid-cell-1-0"));
  fireEvent.mouseEnter(screen.getByTestId("grid-cell-2-0"));
  expect(publishes).toBe(before);
  expect(resolvers).toHaveLength(0);
  fireEvent.mouseUp(document);
  await vi.waitFor(() => expect(resolvers).toHaveLength(1));
  expect(publishes).toBe(before + 1);
  expect(screen.queryByTestId("result-selection")).toBeNull();
  resolvers[0]!({ status: "ok", data: { kind: "Numeric", count: 3, nonNull: 3, sum: "6", avg: "2.000000", min: "1", max: "3" } });
  expect((await screen.findByTestId("result-selection")).textContent).toContain("Sum 6");
  expect(publishes).toBe(before + 2);
  expect(resolvers).toHaveLength(1);
});

const sortingQuery = (onSort: (column: string, additive: boolean) => void) => ({
  sort: [],
  filtered: new Set<string>(),
  onSort,
  onFilterColumn: () => {},
  onFilterValue: () => {},
});

it("sorts by the selected cell's column with ⌥S and adds it to the sort with ⇧⌥S", async () => {
  const user = userEvent.setup();
  const onSort = vi.fn();
  render(<ResultGrid meta={queryResult([["1", "42"]])} done query={sortingQuery(onSort)} editing={null} />);
  await user.click(await screen.findByText("42"));
  await user.keyboard("{Alt>}s{/Alt}");
  await user.keyboard("{Shift>}{Alt>}s{/Alt}{/Shift}");
  expect(onSort.mock.calls).toEqual([
    ["customer_id", false],
    ["customer_id", true],
  ]);
});

it("calls the latest sort when the pane renders the same sort with new callbacks", async () => {
  const user = userEvent.setup();
  const meta = queryResult([["1", "42"]]);
  const first = vi.fn();
  const second = vi.fn();
  const { rerender } = render(<ResultGrid meta={meta} done query={sortingQuery(first)} editing={null} />);
  await screen.findByText("42");
  rerender(<ResultGrid meta={meta} done query={sortingQuery(second)} editing={null} />);
  await user.click(screen.getByTestId("grid-header-0"));
  expect([first.mock.calls.length, second.mock.calls]).toEqual([0, [["id", false]]]);
});

it("widens and narrows the selected cell's column with ⌥→ and ⌥←, and from the menu", async () => {
  const user = userEvent.setup();
  render(<ResultGrid meta={queryResult([["1", "42"]])} done query={null} editing={null} />);
  await user.click(await screen.findByText("42"));
  const width = () => parseFloat(screen.getByTestId("grid-header-1").style.width);
  const start = width();
  await user.keyboard("{Alt>}{ArrowRight}{ArrowRight}{/Alt}");
  expect(width()).toBe(start + 32);
  await user.keyboard("{Alt>}{ArrowLeft}{/Alt}");
  expect(width()).toBe(start + 16);
  expect(screen.getByTestId("grid-cell-0-1").getAttribute("aria-selected")).toBe("true");

  await user.pointer({ keys: "[MouseRight]", target: screen.getByText("42") });
  expect((await screen.findByTestId("grid-menu-widen")).getAttribute("aria-keyshortcuts")).toBe("Alt+ArrowRight");
  await user.click(screen.getByTestId("grid-menu-reset-width"));
  expect(width()).toBe(start);
});

it("stops following the pointer when it unmounts mid resize", async () => {
  const meta = queryResult([["1", "42"]]);
  const removed = vi.spyOn(window, "removeEventListener");
  const { unmount } = render(<ResultGrid meta={meta} done query={null} editing={null} />);
  await screen.findByText("42");
  fireEvent.mouseDown(screen.getByTestId("grid-resize-1"), { clientX: 100 });
  unmount();
  expect(removed.mock.calls.map(([type]) => type)).toEqual(expect.arrayContaining(["mousemove", "mouseup"]));
});

it("names the editor, the full-value button, and each row's staged state for assistive tech", async () => {
  const user = userEvent.setup();
  const meta = queryResult(
    [
      ["1", "4200"],
      ["2", "4300"],
      ["3", "4400"],
    ],
    { cut: [[2, 1]], cell: () => "44000000" },
  );
  useTableEdits
    .getState()
    .edit("q1", (e) => setCell(markDeleted(e, [[{ column: "id", value: "2" }]], true), [{ column: "id", value: "1" }], "customer_id", "7", "4200"));
  useTableEdits.getState().addRow("q1");
  render(<ResultGrid meta={meta} done query={null} editing={EDITING} />);
  await screen.findByText("7");
  expect([0, 1, 3].map((row) => screen.getByTestId(`grid-rownum-${row}`).textContent)).toEqual(["1, changed", "2, marked for deletion", "+New row"]);
  expect(screen.getByTestId("grid-cell-1-1").className).toContain("text-dim line-through");
  expect(screen.getByRole("button", { name: "Show the full value" })).toBe(screen.getByTestId("grid-cut-2-1"));
  await user.dblClick(screen.getByText("7"));
  expect(screen.getByRole("textbox", { name: "customer_id" })).toBe(screen.getByTestId("grid-editor"));
});

const profileOk = (profile: ColumnProfile) => ({ status: "ok" as const, data: profile });
const idProfile: ColumnProfile = {
  rows: 6915,
  nulls: 0,
  distinct: 6915,
  summary: {
    kind: "Numeric",
    min: "1",
    mean: "3458.000000",
    max: "6915",
    histogram: [
      { lo: 1, hi: 3458, count: 3457 },
      { lo: 3458, hi: 6915, count: 3458 },
    ],
  },
};
const statusProfile: ColumnProfile = {
  rows: 4,
  nulls: 1,
  distinct: 2,
  summary: {
    kind: "Values",
    top: [
      { value: "paid", count: 2 },
      { value: "shipped", count: 1 },
    ],
  },
};
const STATUS_COLUMNS = [
  { name: "id", attnum: 1 },
  { name: "status", attnum: null },
];

it("profiles a query result's column from its header and closes on Escape", async () => {
  const user = userEvent.setup();
  const profile = vi.spyOn(commands, "resultProfile").mockResolvedValue(profileOk(idProfile));
  const meta = queryResult([["1", "42"]]);
  render(<ResultGrid meta={meta} done query={null} editing={null} />);
  await user.click(await screen.findByTestId("grid-header-0"));
  const popover = await screen.findByTestId("column-profile");
  expect(profile).toHaveBeenCalledWith(meta.ref, 0);
  await within(popover).findByText("From all 6,915 stored rows.");
  const stats = Object.fromEntries([...popover.querySelectorAll("dt")].map((dt) => [dt.textContent, dt.nextElementSibling?.textContent]));
  expect(stats).toEqual({ Rows: "6,915", Nulls: "0", Distinct: "6,915", Min: "1", Mean: "3,458.000000", Max: "6,915" });
  expect(within(popover).getByRole("img", { name: "Histogram of 2 bins" })).toBeTruthy();
  fireEvent.mouseEnter(screen.getByTestId("profile-bin-1"));
  expect(screen.getByTestId("profile-bin-tooltip").textContent).toBe(`${(3458).toLocaleString()} – ${(6915).toLocaleString()} · 3,458 rows`);
  await user.keyboard("{Escape}");
  await waitFor(() => expect(screen.queryByTestId("column-profile")).toBeNull());
});

it("closes the profile when its header is clicked again or a click lands outside it", async () => {
  const user = userEvent.setup();
  vi.spyOn(commands, "resultProfile").mockResolvedValue(profileOk(idProfile));
  render(<ResultGrid meta={queryResult([["1", "42"]])} done query={null} editing={null} />);
  await user.click(await screen.findByTestId("grid-header-0"));
  await screen.findByText("From all 6,915 stored rows.");
  await user.click(screen.getByTestId("grid-header-0"));
  await waitFor(() => expect(screen.queryByTestId("column-profile")).toBeNull());
  await user.click(screen.getByTestId("grid-header-0"));
  await screen.findByTestId("column-profile");
  await user.click(screen.getByText("42"));
  await waitFor(() => expect(screen.queryByTestId("column-profile")).toBeNull());
});

it("lists a text column's most frequent values with their counts", async () => {
  const user = userEvent.setup();
  vi.spyOn(commands, "resultProfile").mockResolvedValue(profileOk(statusProfile));
  render(<ResultGrid meta={queryResult([["1", "paid"]], { columns: STATUS_COLUMNS })} done query={null} editing={null} />);
  await user.click(await screen.findByTestId("grid-header-1"));
  const values = await screen.findByRole("list", { name: "Most frequent values" });
  expect([...values.querySelectorAll("li")].map((li) => li.textContent)).toEqual(["paid2", "shipped1"]);
  const popover = screen.getByTestId("column-profile");
  expect(within(popover).queryByText("Min")).toBeNull();
  expect(within(popover).getByText("From all 4 stored rows.")).toBeTruthy();
});

it("says a truncated result's profile covers only the stored rows", async () => {
  const user = userEvent.setup();
  vi.spyOn(commands, "resultProfile").mockResolvedValue(profileOk({ ...statusProfile, rows: 2000 }));
  const meta = { ...queryResult([["1", "paid"]], { columns: STATUS_COLUMNS }), rowsSeen: 6915, truncated: "RowLimit" as const };
  render(<ResultGrid meta={meta} done query={null} editing={null} />);
  await user.click(await screen.findByTestId("grid-header-1"));
  expect(await screen.findByText("From the 2,000 stored rows of 6,915 read (row limit).")).toBeTruthy();
});

it("keeps sorting on a table tab's header and profiles from its right-click menu", async () => {
  const user = userEvent.setup();
  const onSort = vi.fn();
  vi.spyOn(commands, "resultProfile").mockResolvedValue(profileOk({ ...statusProfile, rows: 100 }));
  render(<ResultGrid meta={queryResult([["1", "paid"]], { columns: STATUS_COLUMNS })} done query={sortingQuery(onSort)} editing={null} />);
  await user.click(await screen.findByTestId("grid-header-1"));
  expect(onSort.mock.calls).toEqual([["status", false]]);
  expect(screen.queryByTestId("column-profile")).toBeNull();
  await user.pointer({ keys: "[MouseRight]", target: screen.getByTestId("grid-header-1") });
  await user.click(await screen.findByRole("menuitem", { name: "Profile column" }));
  expect(await screen.findByText("From the 100 rows on this page.")).toBeTruthy();
});

it("profiles the selected cell's column from the cell menu, which the keyboard opens", async () => {
  const user = userEvent.setup();
  const profile = vi.spyOn(commands, "resultProfile").mockResolvedValue(profileOk(statusProfile));
  const meta = queryResult([["1", "paid"]], { columns: STATUS_COLUMNS });
  render(<ResultGrid meta={meta} done query={null} editing={null} />);
  await user.click(await screen.findByText("paid"));
  await user.keyboard("{Shift>}{F10}{/Shift}");
  await user.click(await screen.findByRole("menuitem", { name: "Profile status" }));
  await screen.findByTestId("column-profile");
  expect(profile).toHaveBeenCalledWith(meta.ref, 1);
});

it("closes quietly when the result is gone", async () => {
  const user = userEvent.setup();
  vi.spyOn(commands, "resultProfile").mockResolvedValue({ status: "error", error: { kind: "Gone" } });
  render(<ResultGrid meta={queryResult([["1", "42"]])} done query={null} editing={null} />);
  await user.click(await screen.findByTestId("grid-header-0"));
  await waitFor(() => expect(screen.queryByTestId("column-profile")).toBeNull());
});

it("shows the reply to the latest profile request when an earlier one arrives later", async () => {
  const user = userEvent.setup();
  const replies: ((value: { status: "ok"; data: ColumnProfile }) => void)[] = [];
  vi.spyOn(commands, "resultProfile").mockImplementation(() => new Promise((resolve) => replies.push(resolve)));
  render(<ResultGrid meta={queryResult([["1", "paid"]], { columns: STATUS_COLUMNS })} done query={null} editing={null} />);
  await user.click(await screen.findByTestId("grid-header-1"));
  expect(await screen.findByText("Profiling…")).toBeTruthy();
  await user.click(screen.getByTestId("grid-header-0"));
  await waitFor(() => expect(replies).toHaveLength(2));
  replies[1]!(profileOk(idProfile));
  replies[0]!(profileOk(statusProfile));
  expect(await screen.findByText("From all 6,915 stored rows.")).toBeTruthy();
  expect(screen.queryByRole("list", { name: "Most frequent values" })).toBeNull();
});

it("names a bin's edges finely enough to tell a tiny range from 0, without float noise", async () => {
  const user = userEvent.setup();
  const tiny = (lo: number, hi: number, count: number) => ({ lo, hi, count });
  vi.spyOn(commands, "resultProfile").mockResolvedValue(
    profileOk({
      ...idProfile,
      summary: {
        kind: "Numeric",
        min: "0.0000001",
        mean: "0.0000002",
        max: "0.0000003",
        histogram: [tiny(0.0000001, 0.0000002, 1), tiny(0.0000002, 0.0000003, 1)],
      },
    }),
  );
  render(<ResultGrid meta={queryResult([["1", "42"]])} done query={null} editing={null} />);
  await user.click(await screen.findByTestId("grid-header-0"));
  fireEvent.mouseEnter(await screen.findByTestId("profile-bin-0"));
  expect(screen.getByTestId("profile-bin-tooltip").textContent).toBe("0.0000001 – 0.0000002 · 1 row");
});
