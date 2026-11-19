// @vitest-environment jsdom
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterAll, afterEach, beforeAll, beforeEach, expect, it, vi } from "vitest";
import type { Catalog, ConsoleState, ResultMeta, RowPage, SqlTypeRef } from "../../ipc/bindings";
import { COMMANDS } from "../../shell/commands";
import { buildCatalogView, useCatalog } from "../../state/catalog";
import { dbKey } from "../../state/dbKey";
import { useLive } from "../../state/live";
import { dropRowCaches } from "../../state/rowCache";
import { NO_EDITS, stagingSettled, toChanges, useTableEdits } from "../../state/tableEdits";
import { useWorkspace } from "../../state/workspace";
import { dropEditor, seedEditor } from "../editor/editorStates";
import { tableTarget } from "../../testing/editTarget";
import { shopCatalog } from "../../testing/shopCatalog";
import { idleConsole, showTableRows } from "../../testing/tableRun";
import { ResultGrid, type GridEditing } from "./ResultGrid";

const meta: ResultMeta = {
  databaseKind: "Postgres",
  ref: { tabId: "t1", runId: 1, index: 0 },
  db: { connectionId: "c1", database: "shop" },
  columns: [
    { name: "id", type: { display: "bigint", category: "Integer" }, source: { tableOid: 16_406, attnum: 1 } },
    { name: "status", type: { display: "text", category: "Text" }, source: { tableOid: 16_406, attnum: 3 } },
  ],
  rowCount: 1,
  rowsSeen: 1,
  truncated: null,
  elapsedMs: 1,
  allRows: { kind: "Table" },
  repeatedRead: null,
};
const editing: GridEditing = { pkColumns: ["id"], writableColumns: new Set(["status"]) };
const T1 = { db: meta.db, schema: "public", table: "orders" };
const target = tableTarget(T1, ["id"]);

beforeAll(() => {
  // jsdom lays nothing out, and the virtualizer renders no rows into a zero-size viewport.
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
});

afterAll(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

beforeEach(() => {
  mockIPC((cmd): RowPage => {
    if (cmd === "result_page") return { start: 0, rows: [["1", "paid"]], cut: [] };
    throw "not served in this test";
  });
});

afterEach(() => {
  cleanup();
  clearMocks();
  dropRowCaches("t1");
  dropEditor("t1");
  useTableEdits.setState(useTableEdits.getInitialState(), true);
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
  useCatalog.setState(useCatalog.getInitialState(), true);
});

const pending = () => useTableEdits.getState().byTab.t1 ?? NO_EDITS;

async function typeIntoStatus(user: ReturnType<typeof userEvent.setup>, text: string) {
  render(<ResultGrid meta={meta} done query={null} target={target} editing={editing} />);
  await user.dblClick(await screen.findByText("paid"));
  const editor = screen.getByTestId("grid-editor");
  await user.clear(editor);
  await user.type(editor, text);
}

it("drops the draft when Escape cancels the edit", async () => {
  const user = userEvent.setup();
  await typeIntoStatus(user, "shipped");
  await user.keyboard("{Escape}");
  expect(screen.queryByTestId("grid-editor")).toBeNull();
  expect(pending().updates).toEqual({});
  expect(screen.getByTestId("result-grid").getAttribute("data-pending")).toBe("0");
  expect(screen.getByText("paid")).toBeTruthy();
});

it("stages the draft when Enter commits the edit", async () => {
  const user = userEvent.setup();
  await typeIntoStatus(user, "shipped");
  await user.keyboard("{Enter}");
  expect(screen.queryByTestId("grid-editor")).toBeNull();
  expect(Object.values(pending().updates)).toEqual([{ key: [{ column: "id", value: "1" }], values: { status: "shipped" } }]);
  expect(screen.getByTestId("result-grid").getAttribute("data-pending")).toBe("1");
});

it("opens results find from a cell editor through its normal draft finish path", async () => {
  const user = userEvent.setup();
  await typeIntoStatus(user, "shipped");
  await user.keyboard("{Control>}f{/Control}");
  expect(screen.queryByTestId("grid-editor")).toBeNull();
  expect(Object.values(pending().updates)).toEqual([{ key: [{ column: "id", value: "1" }], values: { status: "shipped" } }]);
  const input = screen.getByRole("textbox", { name: "Find in results" });
  await vi.waitFor(() => expect(document.activeElement).toBe(input));
  expect(screen.getByText(/Saved\/retained values only/)).toBeTruthy();
});

it("holds Submit while a typed-over SQL Server cell loads its full original value", async () => {
  const user = userEvent.setup();
  let full!: (value: string) => void;
  mockIPC((cmd): RowPage | Promise<string> => {
    if (cmd === "result_page") return { start: 0, rows: [["1", "pai"]], cut: [[0, 1]] };
    if (cmd === "result_cell") return new Promise((resolve) => (full = resolve));
    throw "not served in this test";
  });
  render(<ResultGrid meta={{ ...meta, databaseKind: "SqlServer" }} done query={null} target={target} editing={{ ...editing, rowCheck: { kind: "Values" } }} />);
  await user.click(await screen.findByText("pai"));
  await user.keyboard("x{Enter}");
  expect(screen.queryByTestId("grid-editor")).toBeNull();
  let settled = false;
  const held = stagingSettled("t1").then(() => (settled = true));
  await vi.waitFor(() => expect(full).toBeTypeOf("function"));
  expect([settled, pending().updates]).toEqual([false, {}]);
  full("paid in full");
  await held;
  expect(toChanges(pending(), "dbo", "t", { kind: "Values" }).changes.updates).toEqual([
    { key: [{ column: "id", value: "1" }], set: [{ column: "status", value: "x" }], expect: [{ column: "status", value: "paid in full" }] },
  ]);
});

it("stops a running SQL Server Submit when Esc is pressed in the grid", async () => {
  const user = userEvent.setup();
  const cancels: unknown[] = [];
  mockIPC((cmd, args): RowPage | null => {
    if (cmd === "result_page") return { start: 0, rows: [["1", "paid"]], cut: [] };
    if (cmd !== "console_cancel") throw "not served in this test";
    cancels.push(args);
    return null;
  });
  render(<ResultGrid meta={{ ...meta, databaseKind: "SqlServer" }} done query={null} target={target} editing={editing} />);
  await user.click(await screen.findByText("paid"));
  await user.keyboard("{Escape}");
  expect(cancels).toEqual([]);
  useLive.setState({ consoles: { t1: { work: "Edit" } as ConsoleState } });
  await user.keyboard("{Escape}");
  expect(cancels).toEqual([{ tabId: "t1" }]);
});

function gridWith(value: string | null, type: SqlTypeRef = { display: "text", category: "Text" }, extra: Partial<ResultMeta> = {}) {
  mockIPC((cmd) => {
    if (cmd === "result_page") return { start: 0, rows: [["1", value]], cut: [] };
    if (cmd === "result_find") return { count: 0, target: null };
    throw new Error(`Unexpected ${cmd}`);
  });
  render(
    <>
      <ResultGrid
        meta={{ ...meta, ...extra, columns: [meta.columns[0]!, { ...meta.columns[1]!, type }] }}
        done
        query={null}
        target={target}
        editing={editing}
      />
      <button type="button">Outside destination</button>
    </>,
  );
}
async function openStatus(user: ReturnType<typeof userEvent.setup>, row = 0) {
  await user.dblClick(await screen.findByTestId(`grid-cell-${row}-1`));
}
const stagedStatus = () => Object.values(pending().updates)[0]?.values.status;

it("leaves untouched NULL intact but stages deliberately edited empty text", async () => {
  const user = userEvent.setup();
  gridWith(null);
  await openStatus(user);
  await user.keyboard("{Enter}");
  expect(pending().updates).toEqual({});
  await openStatus(user);
  await user.type(screen.getByTestId("grid-editor"), "x{Backspace}{Enter}");
  expect(stagedStatus()).toBe("");
});

it("keeps untouched inserted DEFAULT omitted and deliberately empty inserted text present", async () => {
  const user = userEvent.setup();
  useTableEdits.getState().addRow("t1", T1);
  gridWith("paid");
  await openStatus(user, 1);
  await user.keyboard("{Enter}");
  expect(pending().inserts[0]?.values).toEqual({});
  await openStatus(user, 1);
  await user.type(screen.getByTestId("grid-editor"), "x{Backspace}{Tab}");
  expect(pending().inserts[0]?.values).toEqual({ status: "" });
  expect(document.activeElement).toBe(screen.getByTestId("result-grid"));
});

it("captures rapid grid keystrokes arriving before focus and stages a huge numeric string exactly", async () => {
  const user = userEvent.setup();
  gridWith("10", { display: "numeric(38,18)", category: "Numeric" });
  await user.click(await screen.findByTestId("grid-cell-0-1"));
  const grid = screen.getByTestId("result-grid");
  act(() => {
    fireEvent.keyDown(grid, { key: "9" });
    fireEvent.keyDown(grid, { key: "8" });
  });
  expect(screen.getByTestId("grid-editor").getAttribute("inputmode")).toBe("decimal");
  expect(screen.getByTestId("grid-editor")).toHaveProperty("value", "98");
  const huge = "90071992547409931234567890.000000000000000001";
  fireEvent.change(screen.getByTestId("grid-editor"), { target: { value: huge } });
  await user.keyboard("{Enter}");
  expect(stagedStatus()).toBe(huge);
});

it("keeps portal focus internal, popup Tab within fields, and outside focus on its destination with one stage", async () => {
  const user = userEvent.setup();
  gridWith("before");
  await openStatus(user);
  await user.click(screen.getByRole("button", { name: "Expand value" }));
  const exact = screen.getByRole("textbox", { name: "Exact status" });
  expect(document.activeElement).toBe(exact);
  await user.type(exact, " after");
  await user.tab();
  expect(document.activeElement).toBe(screen.getByRole("button", { name: "Cancel" }));
  expect(pending().updates).toEqual({});
  const stage = vi.spyOn(useTableEdits.getState(), "edit");
  await user.click(screen.getByRole("button", { name: "Outside destination" }));
  expect(stagedStatus()).toBe("before after");
  expect(screen.queryByRole("textbox", { name: "Exact status" })).toBeNull();
  expect(document.activeElement).toBe(screen.getByRole("button", { name: "Outside destination" }));
  expect(stage).toHaveBeenCalledTimes(1);
  stage.mockRestore();
});

it("cancels the whole popup session on Escape and uses inline Tab for adjacent grid navigation", async () => {
  const user = userEvent.setup();
  gridWith("paid");
  await openStatus(user);
  await user.click(screen.getByRole("button", { name: "Expand value" }));
  fireEvent.change(screen.getByRole("textbox", { name: "Exact status" }), { target: { value: "discard" } });
  await user.keyboard("{Escape}");
  expect(pending().updates).toEqual({});
  await openStatus(user);
  fireEvent.change(screen.getByTestId("grid-editor"), { target: { value: "keep" } });
  await user.tab({ shift: true });
  expect(stagedStatus()).toBe("keep");
  expect(screen.getByTestId("grid-cell-0-0").getAttribute("aria-selected")).toBe("true");
});

it("blocks invalid JSON outside presses, Find and save, then formats exact tokens and stages before save", async () => {
  const user = userEvent.setup();
  gridWith('{"a":1}', { display: "json", category: "Json" });
  const save = COMMANDS.find((command) => command.id === "save")!;
  const seen: unknown[] = [];
  const run = vi.spyOn(save, "run").mockImplementation(() => {
    seen.push(stagedStatus());
  });
  await openStatus(user);
  await user.click(screen.getByRole("button", { name: "Expand value" }));
  const exact = screen.getByRole("textbox", { name: "Exact status" });
  fireEvent.change(exact, { target: { value: '{"bad":' } });
  const outsideCommand = vi.fn();
  screen.getByRole("button", { name: "Outside destination" }).addEventListener("click", outsideCommand);
  await user.click(screen.getByRole("button", { name: "Outside destination" }));
  expect(outsideCommand).not.toHaveBeenCalled();
  expect(screen.getByRole("alert").textContent).toMatch(/expected/i);
  expect(screen.getByRole("button", { name: "Apply" }).hasAttribute("disabled")).toBe(true);
  await user.click(exact);
  await user.keyboard("{Control>}f{/Control}{Control>}s{/Control}");
  expect(screen.queryByRole("textbox", { name: "Find in results" })).toBeNull();
  expect(run).not.toHaveBeenCalled();
  expect(pending().updates).toEqual({});
  const source = '{"n":900719925474099312345,"n":1e+999,"s":"\\u0061"}';
  fireEvent.change(exact, { target: { value: source } });
  await user.click(screen.getByRole("button", { name: "Format" }));
  await user.click(exact);
  await user.keyboard("{Control>}s{/Control}");
  expect(run).toHaveBeenCalledTimes(1);
  expect(seen).toEqual([stagedStatus()]);
  expect(stagedStatus()).toContain('"n": 900719925474099312345');
  expect(stagedStatus()).toContain('"n": 1e+999');
  expect(stagedStatus()).toContain('"s": "\\u0061"');
  run.mockRestore();
});

it("opens Find from an expanded textarea after staging and preserves multiline clipboard text", async () => {
  const user = userEvent.setup();
  gridWith("before");
  await openStatus(user);
  const inline = screen.getByTestId("grid-editor");
  await user.clear(inline);
  await user.paste("first\n  second\nthird");
  expect(screen.getByRole("textbox", { name: "Exact status" })).toBeTruthy();
  await user.keyboard("{Control>}f{/Control}");
  expect(stagedStatus()).toBe("first\n  second\nthird");
  await vi.waitFor(() => expect(document.activeElement).toBe(screen.getByRole("textbox", { name: "Find in results" })));
});

it.each(["2026-11-18 14:30:00.123456+01", "2026-11-18T14:30:00.1234567-03:30"])(
  "changes only the date of %s and preserves incomplete lexical fields",
  async (source) => {
    const user = userEvent.setup();
    gridWith(source, { display: "timestamptz", category: "Temporal" });
    await openStatus(user);
    await user.click(screen.getByRole("button", { name: "Open date and time picker" }));
    expect(screen.getByRole("textbox", { name: "Fraction" })).toHaveProperty("value", source.includes("1234567") ? "1234567" : "123456");
    await user.click(screen.getByRole("gridcell", { name: "2026-11-19" }));
    const time = screen.getByRole("textbox", { name: "Time" });
    fireEvent.change(time, { target: { value: "14:" } });
    await user.click(screen.getByRole("button", { name: "Outside destination" }));
    expect(time).toHaveProperty("value", "14:");
    expect(screen.getByRole("alert").textContent).toMatch(/complete 24-hour/);
    expect(pending().updates).toEqual({});
    fireEvent.change(time, { target: { value: "14:30:00" } });
    await user.click(screen.getByRole("button", { name: "Apply" }));
    expect(stagedStatus()).toBe("2026-11-19" + source.slice(10));
  },
);

it("navigates the Monday calendar and month/year jumps without changing the value until a date is chosen", async () => {
  const user = userEvent.setup();
  gridWith("2024-01-31", { display: "date", category: "Temporal" });
  await openStatus(user);
  await user.click(screen.getByRole("button", { name: "Open date and time picker" }));
  expect(screen.queryByRole("textbox", { name: "Time" })).toBeNull();
  expect(screen.getAllByRole("columnheader").some((el) => el.textContent === "Mo")).toBe(true);
  const day = within(screen.getByRole("grid", { name: "Calendar" })).getByRole("gridcell", { name: "2024-01-31" });
  await user.click(day);
  await user.keyboard("{PageDown}");
  await vi.waitFor(() => expect(document.activeElement).toBe(screen.getByRole("gridcell", { name: "2024-02-29" })));
  await user.keyboard("{ArrowLeft}");
  await vi.waitFor(() => expect(document.activeElement).toBe(screen.getByRole("gridcell", { name: "2024-02-28" })));
  await user.keyboard("{Enter}");
  await user.click(screen.getByRole("button", { name: "Choose month and year" }));
  await user.selectOptions(screen.getByRole("combobox", { name: "Month" }), "12");
  const year = screen.getByRole("textbox", { name: "Year" });
  await user.clear(year);
  await user.type(year, "1");
  await user.click(screen.getByRole("button", { name: "Close month and year" }));
  expect(screen.getByRole("button", { name: "Choose month and year" }).textContent).toBe("December 1");
  await user.click(screen.getByRole("button", { name: "Apply" }));
  expect(stagedStatus()).toBe("2024-02-28");
});

it.each(["infinity", "0001-01-01 00:00:00+00 BC", "2026-01-01 24:00:00+00"])(
  "opens unsupported temporal %s as exact source without normalizing it",
  async (source) => {
    const user = userEvent.setup();
    gridWith(source, { display: "timestamptz", category: "Temporal" });
    await openStatus(user);
    await user.click(screen.getByRole("button", { name: "Open date and time picker" }));
    expect(screen.getByRole("textbox", { name: "Exact status" })).toHaveProperty("value", source);
    expect(screen.queryByRole("grid", { name: "Calendar" })).toBeNull();
    await user.click(screen.getByRole("button", { name: "Apply" }));
    expect(pending().updates).toEqual({});
  },
);

it.each(["Postgres", "SqlServer"] as const)("offers explicit boolean choices for NULL in %s without an implicit false", async (databaseKind) => {
  const user = userEvent.setup();
  gridWith(null, { display: databaseKind === "Postgres" ? "boolean" : "bit", category: "Bool" }, { databaseKind });
  await openStatus(user);
  expect(screen.getByRole("button", { name: "True" }).getAttribute("aria-pressed")).toBe("false");
  expect(screen.getByRole("button", { name: "False" }).getAttribute("aria-pressed")).toBe("false");
  await user.keyboard("{Enter}");
  expect(pending().updates).toEqual({});
  await openStatus(user);
  await user.click(screen.getByRole("button", { name: "False" }));
  await user.click(screen.getByRole("button", { name: "Outside destination" }));
  expect(stagedStatus()).toBe(databaseKind === "Postgres" ? "f" : "0");
});

it("searches actual enum labels and stages the empty label from NULL", async () => {
  const user = userEvent.setup();
  const catalog: Catalog = {
    databaseKind: "Postgres",
    db: meta.db,
    version: 1,
    defaultSchema: "public",
    extensions: [],
    schemas: [
      {
        name: "public",
        quoted: "public",
        system: false,
        tableCount: 0,
        relations: [],
        functions: [],
        sequences: [],
        synonyms: [],
        types: [{ oid: 42, name: "status", quoted: "status", kind: { kind: "Enum", labels: ["Paid", "", "AWAITING"] } }],
      },
    ],
  };
  useCatalog.setState({ views: { [dbKey(meta.db)]: buildCatalogView(catalog) } });
  gridWith(null, { display: "public.status", category: "Enum" });
  await openStatus(user);
  await user.click(screen.getByRole("button", { name: "Choose value" }));
  const search = screen.getByRole("textbox", { name: "Search values" });
  await user.type(search, "await");
  expect(screen.getAllByRole("option").map((el) => el.textContent)).toEqual(["AWAITING"]);
  await user.clear(search);
  await user.click(screen.getByRole("option", { name: "Empty string" }));
  await user.click(screen.getByRole("button", { name: "Apply" }));
  expect(stagedStatus()).toBe("");
});

it("fetches the full cut source before opening a popup and retains the full original", async () => {
  const user = userEvent.setup();
  const source = "first\n" + "x".repeat(2000);
  mockIPC((cmd) => {
    if (cmd === "result_page") return { start: 0, rows: [["1", "first"]], cut: [[0, 1]] };
    if (cmd === "result_cell") return source;
    throw new Error(`Unexpected ${cmd}`);
  });
  render(<ResultGrid meta={{ ...meta, databaseKind: "SqlServer" }} done query={null} target={target} editing={{ ...editing, rowCheck: { kind: "Values" } }} />);
  await openStatus(user);
  await user.click(await screen.findByRole("button", { name: "Expand value" }));
  expect(screen.getByRole("textbox", { name: "Exact status" })).toHaveProperty("value", source);
  fireEvent.change(screen.getByRole("textbox", { name: "Exact status" }), { target: { value: source + "!" } });
  await user.click(screen.getByRole("button", { name: "Apply" }));
  expect(toChanges(pending(), "dbo", "t", { kind: "Values" }).changes.updates[0]?.expect).toEqual([{ column: "status", value: source }]);
});

it("stages the current popup value before the real save command submits its exact IPC change", async () => {
  const user = userEvent.setup();
  const applied: unknown[] = [];
  mockIPC((cmd, args) => {
    if (cmd === "result_page") return { start: 0, rows: [["1", "paid"]], cut: [] };
    if (cmd === "script_params") return [];
    if (cmd === "table_script") return { text: "SELECT * FROM public.orders", acceptedTexts: ["SELECT * FROM public.orders"] };
    if (cmd === "table_apply") {
      applied.push(args);
      return { committed: true };
    }
    return null;
  });
  seedEditor("t1", "SELECT * FROM public.orders");
  useWorkspace.getState().addTab({
    id: "t1",
    title: "orders",
    console: { db: meta.db, mode: "AutoCommit" },
    table: { schema: "public", table: "orders", page: 0, query: { where: "", filters: [], orderBy: "" } },
  });
  useLive.setState({ consoles: { t1: idleConsole("t1", meta.db) } });
  showTableRows("t1", shopCatalog("c1", 1), "public", "orders");
  render(<ResultGrid meta={meta} done query={null} target={target} editing={editing} />);
  await openStatus(user);
  await user.click(screen.getByRole("button", { name: "Expand value" }));
  fireEvent.change(screen.getByRole("textbox", { name: "Exact status" }), { target: { value: "first\nsecond" } });
  await user.keyboard("{Control>}s{/Control}");
  await vi.waitFor(() =>
    expect(applied).toEqual([
      {
        tabId: "t1",
        confirmed: null,
        changes: {
          schema: "public",
          table: "orders",
          inserts: [],
          updates: [{ key: [{ column: "id", value: "1" }], set: [{ column: "status", value: "first\nsecond" }], expect: [] }],
          deletes: [],
        },
      },
    ]),
  );
  expect(pending().updates).toEqual({});
});

it.each([
  { display: "time(7)", category: "Temporal", value: "14:30:00.1234567", zone: false, date: false },
  { display: "datetime2(7)", category: "Temporal", value: "2026-11-18 14:30:00.1234567", zone: false, date: true },
  { display: "datetimeoffset(7)", category: "Temporal", value: "2026-11-18 14:30:00.1234567+01:00", zone: true, date: true },
] satisfies { display: string; category: "Temporal"; value: string; zone: boolean; date: boolean }[])(
  "shows only relevant temporal fields for $display",
  async ({ display, category, value, zone, date }) => {
    const user = userEvent.setup();
    gridWith(value, { display, category }, { databaseKind: "SqlServer" });
    await openStatus(user);
    await user.click(screen.getByRole("button", { name: "Open date and time picker" }));
    expect(!!screen.queryByRole("textbox", { name: "UTC offset" })).toBe(zone);
    expect(!!screen.queryByRole("grid", { name: "Calendar" })).toBe(date);
    expect(screen.getByRole("textbox", { name: "Fraction" })).toHaveProperty("value", "1234567");
    await user.click(screen.getByRole("button", { name: "Apply" }));
    expect(pending().updates).toEqual({});
  },
);

it("uses Today and Now deliberately, and exact mode can recover a partial temporal draft", async () => {
  const user = userEvent.setup();
  gridWith("2026-11-18 14:30:00.1234567-03:30", { display: "timestamptz", category: "Temporal" });
  await openStatus(user);
  await user.click(screen.getByRole("button", { name: "Open date and time picker" }));
  await user.click(screen.getByRole("button", { name: "Today" }));
  expect(screen.getByRole("textbox", { name: "Time" })).toHaveProperty("value", "14:30:00");
  expect(screen.getByRole("textbox", { name: "UTC offset" })).toHaveProperty("value", "-03:30");
  await user.click(screen.getByRole("button", { name: "Now" }));
  expect(screen.getByRole("textbox", { name: "Fraction" })).toHaveProperty("value", "0000000");
  fireEvent.change(screen.getByRole("textbox", { name: "Time" }), { target: { value: "14:" } });
  await user.click(screen.getByRole("button", { name: "Edit exact value" }));
  expect(screen.getByRole("textbox", { name: "Exact status" })).toHaveProperty("value", expect.stringContaining(" 14:.0000000"));
  fireEvent.change(screen.getByRole("textbox", { name: "Exact status" }), { target: { value: "infinity" } });
  await user.click(screen.getByRole("button", { name: "Apply" }));
  expect(stagedStatus()).toBe("infinity");
});

it.each([
  { display: "text[]", category: "Array", source: '{"a,b","line\\nnext",NULL}' },
  { display: "bytea", category: "Binary", source: "\\x68656c6c6f" },
  { display: "int4range", category: "Range", source: "[100,500)" },
  { display: "custom", category: "Composite", source: '("a,b",42)' },
] satisfies { display: string; category: SqlTypeRef["category"]; source: string }[])(
  "expands $display as exact database syntax",
  async ({ display, category, source }) => {
    const user = userEvent.setup();
    gridWith(source, { display, category });
    await openStatus(user);
    await user.click(screen.getByRole("button", { name: "Expand value" }));
    expect(screen.getByRole("textbox", { name: "Exact status" })).toHaveProperty("value", source);
    fireEvent.change(screen.getByRole("textbox", { name: "Exact status" }), { target: { value: source + " " } });
    await user.click(screen.getByRole("button", { name: "Apply" }));
    expect(stagedStatus()).toBe(source + " ");
  },
);

it("keeps a pristine calendar session unchanged even after month navigation", async () => {
  const user = userEvent.setup();
  gridWith("2026-11-18", { display: "date", category: "Temporal" });
  await openStatus(user);
  await user.click(screen.getByRole("button", { name: "Open date and time picker" }));
  await user.click(screen.getByRole("button", { name: "Next month" }));
  await user.click(screen.getByRole("button", { name: "Previous month" }));
  await user.click(screen.getByRole("button", { name: "Apply" }));
  expect(pending().updates).toEqual({});
});

it("preserves CRLF clipboard source and its caret across expansion, and Cancel discards invalid JSON", async () => {
  const user = userEvent.setup();
  gridWith("{}", { display: "json", category: "Json" });
  await openStatus(user);
  await user.clear(screen.getByTestId("grid-editor"));
  await user.paste('{\r\n"n":1\r\n}');
  const exact = screen.getByRole("textbox", { name: "Exact status" });
  expect(exact).toHaveProperty("selectionStart", '{\n"n":1\n}'.length);
  await user.click(screen.getByRole("button", { name: "Apply" }));
  expect(stagedStatus()).toBe('{\r\n"n":1\r\n}');
  await openStatus(user);
  await user.click(screen.getByRole("button", { name: "Expand value" }));
  fireEvent.change(screen.getByRole("textbox", { name: "Exact status" }), { target: { value: "{" } });
  await user.click(screen.getByRole("button", { name: "Cancel" }));
  expect(screen.queryByRole("textbox", { name: "Exact status" })).toBeNull();
  expect(stagedStatus()).toBe('{\r\n"n":1\r\n}');
});

it("preserves pristine temporal NULL on popup Apply and deliberately empty NULL in exact mode", async () => {
  const user = userEvent.setup();
  gridWith(null, { display: "date", category: "Temporal" });
  await openStatus(user);
  await user.click(screen.getByRole("button", { name: "Open date and time picker" }));
  await user.click(screen.getByRole("button", { name: "Apply" }));
  expect(pending().updates).toEqual({});
  await openStatus(user);
  await user.type(screen.getByTestId("grid-editor"), "x{Backspace}");
  await user.click(screen.getByRole("button", { name: "Open date and time picker" }));
  expect(screen.getByRole("textbox", { name: "Exact status" })).toHaveProperty("value", "");
  await user.click(screen.getByRole("button", { name: "Apply" }));
  expect(stagedStatus()).toBe("");
});

it("reaches both boolean choices by keyboard and stages when leaving the cell", async () => {
  const user = userEvent.setup();
  gridWith(null, { display: "boolean", category: "Bool" });
  await openStatus(user);
  expect(document.activeElement).toBe(screen.getByRole("button", { name: "True" }));
  await user.keyboard("{ArrowRight}");
  expect(document.activeElement).toBe(screen.getByRole("button", { name: "False" }));
  expect(screen.getByRole("button", { name: "False" }).getAttribute("aria-pressed")).toBe("true");
  await user.keyboard("{Tab}");
  expect(stagedStatus()).toBe("f");
});

it("opens a false boolean with its selected choice focused", async () => {
  const user = userEvent.setup();
  gridWith("f", { display: "boolean", category: "Bool" });
  await openStatus(user);
  expect(document.activeElement).toBe(screen.getByRole("button", { name: "False" }));
  await user.keyboard("{ArrowLeft}{Tab}");
  expect(stagedStatus()).toBe("t");
});

it.each(["Control", "Meta"])("applies an expanded draft with %s+Enter", async (modifier) => {
  const user = userEvent.setup();
  gridWith("original", { display: "text", category: "Text" });
  await openStatus(user);
  await user.click(screen.getByRole("button", { name: "Expand value" }));
  await user.clear(screen.getByRole("textbox", { name: "Exact status" }));
  await user.type(screen.getByRole("textbox", { name: "Exact status" }), "first{Enter}second");
  await user.keyboard(`{${modifier}>}{Enter}{/${modifier}}`);
  expect(stagedStatus()).toBe("first\nsecond");
  expect(screen.queryByRole("textbox", { name: "Exact status" })).toBeNull();
});

it("lets an explicit plain-text mode defer JSON validation to the database", async () => {
  const user = userEvent.setup();
  gridWith("{}", { display: "json", category: "Json" });
  await openStatus(user);
  await user.click(screen.getByRole("button", { name: "Expand value" }));
  fireEvent.change(screen.getByRole("textbox", { name: "Exact status" }), { target: { value: "{unfinished" } });
  expect(screen.getByRole("button", { name: "Apply" }).hasAttribute("disabled")).toBe(true);
  await user.click(screen.getByRole("button", { name: "Edit as plain text" }));
  expect(screen.getByRole("button", { name: "Apply" }).hasAttribute("disabled")).toBe(false);
  await user.click(screen.getByRole("button", { name: "Validate JSON" }));
  expect(screen.getByRole("button", { name: "Apply" }).hasAttribute("disabled")).toBe(true);
  await user.click(screen.getByRole("button", { name: "Edit as plain text" }));
  await user.click(screen.getByRole("button", { name: "Apply" }));
  expect(stagedStatus()).toBe("{unfinished");
});
