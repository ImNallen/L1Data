import { undo } from "@codemirror/commands";
import { mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import type {
  ApplyError,
  ConnectionConfig,
  ConsoleState,
  DatabaseKind,
  RunError,
  Settings,
  TableQuery,
  TableScript,
  TabSnapshot,
  TxnError,
  WriteConfirmation,
} from "../../ipc/bindings";
import { connectionConfig } from "../../testing/connections";
import { shopCatalog } from "../../testing/shopCatalog";
import { buildCatalogView, useCatalog } from "../../state/catalog";
import { dbKey } from "../../state/dbKey";
import { useLive } from "../../state/live";
import { markDeleted, pendingCount, setCell, useTableEdits } from "../../state/tableEdits";
import { activeId, useWorkspace, workspaceSnapshot } from "../../state/workspace";
import { gridEditing } from "../results/ResultGrid";
import { dropEditor, editorStateOf, insertAtCursor, moveCursor, seedEditor, textOf } from "./editorStates";
import { finishParams, useParams } from "./paramsState";
import {
  browseTemplate,
  formatActive,
  lastTablePage,
  loadTableOnce,
  openTableTab,
  saveSettings,
  setTabMode,
  runActive,
  runOrChoose,
  explainActive,
  openSqlTab,
  isBrowsing,
  submitEdits,
  revertEdits,
  cancelActive,
  endTransaction,
  installRunBehavior,
  setTablePage,
  updateTableQuery,
} from "./runActions";
import { moveCell } from "../../testing/notebooks";

afterEach(() => {
  vi.unstubAllGlobals();
  for (const id of Object.keys(useWorkspace.getState().tabs)) dropEditor(id);
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
  useCatalog.setState(useCatalog.getInitialState(), true);
  useTableEdits.setState(useTableEdits.getInitialState(), true);
});

const orders = { db: { connectionId: "c1", database: "shop" }, schema: "public", relation: "orders" };
const ordersScript = {
  text: "SELECT * FROM public.orders",
  acceptedTexts: ["SELECT * FROM public.orders", "SELECT * FROM public.orders;"],
} satisfies TableScript;

beforeEach(() => {
  vi.stubGlobal("window", { crypto });
  mockIPC((cmd) => (cmd === "script_params" ? [] : cmd === "table_script" ? ordersScript : null));
});

it("uses the engine's canonical template for bracketed table names", async () => {
  const calls: unknown[] = [];
  mockIPC((cmd, args) => {
    if (cmd === "script_params") return [];
    if (cmd === "table_script") {
      calls.push(args);
      return { text: "SELECT * FROM [Sales].[line items]", acceptedTexts: ["SELECT * FROM [Sales].[line items]"] } satisfies TableScript;
    }
  });
  const db = { connectionId: "mssql", database: "shop" };
  await openTableTab({ db, schema: "Sales", relation: "line items" });
  const tab = useWorkspace.getState().tabs[activeId(useWorkspace.getState())!];
  expect(calls).toEqual([{ db, schema: "Sales", table: "line items" }]);
  expect(tab && textOf(tab.id)).toBe("SELECT * FROM [Sales].[line items]");
  expect(tab && isBrowsing(tab)).toBe(true);
});

it("opens the results when a table opens, new or already in a tab", async () => {
  const ref = { db: { connectionId: "c1", database: "shop" }, schema: "public", relation: "orders" };
  useWorkspace.setState({ layout: { ...useWorkspace.getState().layout, results: false } });
  await openTableTab(ref);
  const tab = useWorkspace.getState().tabs[activeId(useWorkspace.getState())!];
  expect([tab, useWorkspace.getState().layout.results]).toEqual([
    {
      id: tab?.id,
      title: "orders",
      console: { db: { connectionId: "c1", database: "shop" }, mode: "AutoCommit" },
      table: { schema: "public", table: "orders", page: 0, query: { orderBy: "", filters: [], where: "" } },
      file: null,
    },
    true,
  ]);

  useWorkspace.setState({ layout: { ...useWorkspace.getState().layout, results: false } });
  await openTableTab(ref);
  expect(Object.keys(useWorkspace.getState().tabs)).toHaveLength(1);
  expect(useWorkspace.getState().layout.results).toBe(true);
});

it("writes settings one at a time, so the write that lands last carries every change", async () => {
  vi.stubGlobal("window", { crypto });
  const pending: { settings: Settings; finish: () => void }[] = [];
  let inFlight = 0;
  let overlapped = false;
  let persisted: Settings | null = null;
  mockIPC((cmd, args) => {
    if (cmd === "script_params") return [];
    if (cmd !== "settings_set") return;
    const { settings } = args as { settings: Settings };
    overlapped ||= ++inFlight > 1;
    return new Promise<void>((resolve) =>
      pending.push({
        settings,
        finish: () => {
          inFlight--;
          persisted = settings;
          resolve();
        },
      }),
    );
  });
  const flush = () => new Promise((r) => setTimeout(r, 0));

  const saves = Promise.all([saveSettings({ maxRows: 5 }), saveSettings({ colorScheme: "Dark" })]);
  for (await flush(); pending.length > 0; await flush()) pending.pop()!.finish();
  await saves;

  expect({ overlapped, persisted }).toMatchObject({ overlapped: false, persisted: { maxRows: 5, colorScheme: "Dark" } });
});

it("shows a query in the table's open tab, or in a second tab while the first holds staged edits or edited text", async () => {
  const pages: unknown[] = [];
  mockIPC((cmd, args) => {
    if (cmd === "script_params") return [];
    if (cmd === "table_script") return ordersScript;
    if (cmd === "console_run") pages.push((args as { request: unknown }).request);
    return null;
  });
  const byId: TableQuery = { orderBy: "", where: "", filters: [{ column: "id", op: "Eq", value: "7" }] };
  const tables = () => Object.values(useWorkspace.getState().tabs).map((t) => t.table?.query);

  await openTableTab(orders, byId);
  const first = activeId(useWorkspace.getState())!;
  expect(tables()).toEqual([byId]);

  const byTwo: TableQuery = { ...byId, filters: [{ column: "id", op: "Eq", value: "2" }] };
  await openTableTab(orders, byTwo);
  await vi.waitFor(() => expect(pages).toContainEqual({ kind: "tablePage", schema: "public", table: "orders", page: 0, query: byTwo }));
  expect([activeId(useWorkspace.getState()), tables()]).toEqual([first, [byTwo]]);

  useTableEdits.getState().addRow(first);
  await openTableTab(orders, byId);
  const second = activeId(useWorkspace.getState())!;
  expect([second !== first, tables()]).toEqual([true, [byTwo, byId]]);
  expect(pendingCount(useTableEdits.getState().byTab[first]!)).toBe(1);

  insertAtCursor(second, " WHERE true");
  await openTableTab(orders, byTwo);
  expect(tables()).toEqual([byTwo, byId, byTwo]);

  await openTableTab(orders);
  expect([activeId(useWorkspace.getState()), tables()]).toEqual([first, [byTwo, byId, byTwo]]);
});

async function openWithConsole(mode: "AutoCommit" | "Manual" = "AutoCommit") {
  await openTableTab(orders);
  const id = activeId(useWorkspace.getState())!;
  const console: ConsoleState = {
    tabId: id,
    rev: 1,
    config: { connectionId: "c1", database: "shop", schema: null, mode },
    session: { kind: "None" },
    txn: "Idle",
    txnDepth: null,
    work: null,
    export: null,
    import: null,
    run: { kind: "Idle" },
  };
  useLive.setState({ consoles: { [id]: console } });
  return id;
}

it("Last counts on its own tab and loads the exact last table page", async () => {
  const id = await openWithConsole();
  const requests: { cmd: string; args: unknown }[] = [];
  let finish: (count: number) => void = () => {
    throw new Error("count was not requested");
  };
  mockIPC((cmd, args) => {
    if (cmd === "script_params") return [];
    requests.push({ cmd, args });
    if (cmd === "exact_row_count")
      return new Promise<number>((resolve) => {
        finish = resolve;
      });
    if (cmd === "console_run") return { runId: 7, statements: [] };
    return null;
  });
  const pending = lastTablePage(id);
  expect(requests).toEqual([
    { cmd: "exact_row_count", args: { tabId: id, db: orders.db, schema: "public", table: "orders", query: { orderBy: "", filters: [], where: "" } } },
  ]);
  expect(useWorkspace.getState().tabs[id]?.table?.page).toBe(0);
  finish(1251);
  await pending;
  expect(requests[1]).toEqual({
    cmd: "console_run",
    args: {
      tabId: id,
      request: { kind: "tablePage", schema: "public", table: "orders", page: 2, query: { orderBy: "", filters: [], where: "" } },
      confirmed: null,
    },
  });
  expect(useWorkspace.getState().tabs[id]?.table).toMatchObject({ page: 2, exactRowCount: 1251 });
});

it.each(["target", "query", "page", "table", "text", "close", "page size"])("Last discards a deferred count after its %s changes", async (change) => {
  const id = await openWithConsole();
  const requests: string[] = [];
  let finish: (count: number) => void = () => {
    throw new Error("count was not requested");
  };
  mockIPC((cmd) => {
    if (cmd === "script_params") return [];
    requests.push(cmd);
    if (cmd === "exact_row_count")
      return new Promise<number>((resolve) => {
        finish = resolve;
      });
    return null;
  });
  const pending = lastTablePage(id);
  const state = useWorkspace.getState();
  if (change === "target") moveCell(id, { connectionId: "other", database: "reporting" });
  if (change === "query") state.setTableQuery(id, { orderBy: "id DESC", filters: [], where: "id > 10" });
  if (change === "page") state.setTablePage(id, 3);
  if (change === "table") {
    const tab = state.tabs[id]!;
    useWorkspace.setState({ tabs: { ...state.tabs, [id]: { ...tab, table: { ...tab.table!, table: "customers" } } } });
  }
  if (change === "text") insertAtCursor(id, "SELECT 42; ");
  if (change === "close") {
    state.closeTab(id);
    dropEditor(id);
  }
  if (change === "page size") state.setSettings({ ...state.settings, tablePageSize: 100 });
  const current = useWorkspace.getState().tabs[id];
  const text = change === "close" ? undefined : textOf(id);
  const consoles = useLive.getState().consoles;
  finish(1251);
  await pending;
  expect(requests).toEqual(["exact_row_count"]);
  expect(useWorkspace.getState().tabs[id]).toBe(current);
  expect(useLive.getState().consoles).toBe(consoles);
  if (text !== undefined) expect(textOf(id)).toBe(text);
});

it("Last drops its counted page if the query changes during console setup", async () => {
  const id = await openWithConsole();
  useLive.setState({ consoles: {} });
  const requests: string[] = [];
  let configure: () => void = () => {
    throw new Error("console setup was not requested");
  };
  mockIPC((cmd) => {
    if (cmd === "script_params") return [];
    requests.push(cmd);
    if (cmd === "exact_row_count") return 1251;
    if (cmd === "console_configure")
      return new Promise<null>((resolve) => {
        configure = () => resolve(null);
      });
    return null;
  });
  const pending = lastTablePage(id);
  await vi.waitFor(() => expect(requests).toContain("console_configure"));
  useWorkspace.getState().setTableQuery(id, { orderBy: "", filters: [], where: "id = 1" });
  configure();
  await pending;
  expect(requests).toEqual(["exact_row_count", "console_configure"]);
  expect(useWorkspace.getState().tabs[id]?.table).toMatchObject({ page: 0, exactRowCount: undefined, query: { where: "id = 1" } });
});

it("switches a table tab to manual transactions through console_configure", async () => {
  vi.stubGlobal("window", { crypto });
  const calls: { cmd: string; args: unknown }[] = [];
  mockIPC((cmd, args) => {
    if (cmd === "script_params") return [];
    if (cmd === "table_script") return ordersScript;
    calls.push({ cmd, args });
    return null;
  });
  const id = await openWithConsole();
  calls.length = 0;
  await setTabMode(id, "Manual");
  expect(calls).toEqual([{ cmd: "console_configure", args: { tabId: id, config: { connectionId: "c1", database: "shop", schema: null, mode: "Manual" } } }]);
  expect(useWorkspace.getState().tabs[id]?.console.mode).toBe("Manual");
});

it("refuses SQL Server Manual mode before an unopened tab can change commit semantics", async () => {
  useWorkspace.setState({ configs: { ms: { ...connectionConfig("ms", "SQL Server"), databaseKind: "SqlServer" } } });
  const id = openSqlTab({ db: { connectionId: "ms", database: "shop" } });
  const calls: string[] = [];
  mockIPC((cmd) => {
    if (cmd === "script_params") return [];
    calls.push(cmd);
    return null;
  });
  await setTabMode(id, "Manual");
  expect(calls).toEqual([]);
  expect(useWorkspace.getState().tabs[id]?.console.mode).toBe("AutoCommit");
  expect(useWorkspace.getState().toast?.text).toContain("SQL Server Manual mode is unavailable");
});

it.each(["update", "delete"])("submits the original named %s key after PostgreSQL catalog replacement and keeps refused edits", async (change) => {
  const id = await openWithConsole();
  const catalog = shopCatalog("c1", 1);
  const relation = catalog.schemas[1]!.relations.find((r) => r.name === "orders")!;
  relation.columns[1]!.name = "alternate";
  const meta = {
    databaseKind: catalog.databaseKind,
    db: catalog.db,
    columns: relation.columns.map((column) => ({
      name: column.name,
      type: { display: column.typeDisplay, category: column.category },
      source: { tableOid: relation.oid, attnum: column.attnum },
    })),
  };
  const original = buildCatalogView(catalog);
  useCatalog.setState({ views: { [dbKey(catalog.db)]: original } });
  const editing = gridEditing(original, "public", "orders", meta);
  expect(editing?.pkColumns).toEqual(["id"]);
  const originalRow = ["1", "2", "selected", "0", "2026-10-21"];
  const selected = editing!.pkColumns.map((column) => ({ column, value: originalRow[meta.columns.findIndex((c) => c.name === column)] ?? null }));
  useTableEdits
    .getState()
    .edit(id, (edits) => (change === "update" ? setCell(edits, selected, "status", "edited", "selected") : markDeleted(edits, [selected], true)));
  const staged = useTableEdits.getState().byTab[id];

  const refreshed = buildCatalogView({
    ...catalog,
    version: 2,
    schemas: catalog.schemas.map((schema) => ({
      ...schema,
      relations: schema.relations.map((rel) => (rel === relation ? { ...rel, columns: rel.columns.map((c) => ({ ...c, pk: c.name === "alternate" })) } : rel)),
    })),
  });
  useCatalog.setState({ views: { [dbKey(catalog.db)]: refreshed } });
  expect(gridEditing(refreshed, "public", "orders", meta)?.pkColumns).toEqual(["alternate"]);
  const submitted: unknown[] = [];
  mockIPC((cmd, args) => {
    if (cmd === "script_params") return [];
    submitted.push({ cmd, args });
    if (cmd === "table_apply") return Promise.reject({ kind: "NoPrimaryKey" } satisfies ApplyError);
    return null;
  });

  await submitEdits(id);

  expect(submitted).toEqual([
    {
      cmd: "table_apply",
      args: {
        tabId: id,
        changes: {
          schema: "public",
          table: "orders",
          inserts: [],
          updates: change === "update" ? [{ key: [{ column: "id", value: "1" }], set: [{ column: "status", value: "edited" }] }] : [],
          deletes: change === "delete" ? [[{ column: "id", value: "1" }]] : [],
        },
        confirmed: null,
      },
    },
  ]);
  expect(useTableEdits.getState().byTab[id]).toEqual(staged);
  expect(pendingCount(useTableEdits.getState().byTab[id]!)).toBe(1);
  expect(useTableEdits.getState().submitting[id]).toBeUndefined();
  expect(useWorkspace.getState().toast?.text).toContain("primary key");
});

function openScript(text: string, edit: () => unknown) {
  const sent: unknown[] = [];
  mockIPC((cmd, args) => {
    if (cmd === "script_params") return [];
    if (cmd !== "editor_format") return null;
    sent.push(args);
    return edit();
  });
  seedEditor("script", text);
  useWorkspace.getState().addTab({ id: "script", title: "Untitled", console: { db: null, mode: "AutoCommit" }, table: null, file: null });
  return sent;
}

it("formats as its own undo step, apart from the typing before it, and leaves the cursor after the edit", async () => {
  vi.stubGlobal("window", { crypto });
  const sent = openScript("select 1;\nselect a,b from", () => ({ from: 10, to: 27, insert: "SELECT a,\n  b\nFROM t" }));
  moveCursor("script", 25);
  insertAtCursor("script", " t");
  const typed = textOf("script");
  await formatActive();
  let state = editorStateOf("script");
  expect([sent, state.doc.toString(), state.selection.main.head]).toEqual([
    [{ text: typed, databaseKind: "Postgres", target: { kind: "cursor", offset: 27 } }],
    "select 1;\nSELECT a,\n  b\nFROM t",
    30,
  ]);
  undo({ state, dispatch: (tr) => (state = tr.state) });
  expect(state.doc.toString()).toBe(typed);
});

it("toasts a refused format, stays quiet with nothing to format, and drops an edit for text that changed", async () => {
  vi.stubGlobal("window", { crypto });
  let reply: () => unknown = () => Promise.reject({ kind: "Unsafe" });
  openScript("select 1", () => reply());
  await formatActive();
  expect([useWorkspace.getState().toast?.text, textOf("script")]).toEqual(["Couldn't format this statement safely.", "select 1"]);

  useWorkspace.setState({ toast: null });
  reply = () => Promise.reject({ kind: "NothingToFormat" });
  await formatActive();
  expect([useWorkspace.getState().toast, textOf("script")]).toEqual([null, "select 1"]);

  reply = () => {
    insertAtCursor("script", "-- typed\n");
    return { from: 0, to: 8, insert: "SELECT 1" };
  };
  await formatActive();
  expect(textOf("script")).toBe("-- typed\nselect 1");
});

it("loads a table tab's page on first show, and never runs edited text unasked", async () => {
  vi.stubGlobal("window", { crypto });
  const runs: unknown[] = [];
  mockIPC((cmd, args) => {
    if (cmd === "script_params") return [];
    if (cmd === "table_script") return ordersScript;
    if (cmd === "console_run") runs.push((args as { request: unknown }).request);
    if (cmd === "console_run") return { runId: 1, statements: [] };
    return null;
  });
  const query = { orderBy: "total DESC", filters: [{ column: "status", op: "Eq", value: "paid" }], where: "total > 10" } satisfies TableQuery;
  const table = { schema: "public", table: "orders", page: 2, query };
  for (const [id, text] of [
    ["browsing", "SELECT * FROM public.orders"],
    ["edited", "SELECT * FROM public.orders WHERE status = 'pending'"],
  ] as const) {
    seedEditor(id, text);
    useWorkspace.getState().addTab({ id, title: "orders", console: { db: { connectionId: "c1", database: "shop" }, mode: "AutoCommit" }, table, file: null });
  }
  await loadTableOnce("edited");
  await loadTableOnce("browsing");
  await vi.waitFor(() => expect(runs).toHaveLength(1));
  await new Promise((r) => setTimeout(r, 0));
  expect(runs).toEqual([{ kind: "tablePage", schema: "public", table: "orders", page: 2, query }]);
});

it.each(["SELECT * FROM public.orders", "SELECT * FROM public.orders;", "\n SELECT * FROM public.orders\n ", "\t SELECT * FROM public.orders;\n"])(
  "restores a saved browse document %j with its page, filters and order",
  async (text) => {
    const runs: unknown[] = [];
    mockIPC((cmd, args) => {
      if (cmd === "script_params") return [];
      if (cmd === "table_script") return ordersScript;
      if (cmd === "console_run") {
        runs.push(args);
        return { runId: 1, statements: [] };
      }
      return null;
    });
    const id = crypto.randomUUID();
    const query = { orderBy: "total DESC", filters: [{ column: "status", op: "Eq", value: "paid" }], where: "total > 10" } satisfies TableQuery;
    const saved: TabSnapshot = {
      id,
      title: "orders",
      text,
      console: { connectionId: "c1", database: "shop", schema: null, mode: "AutoCommit" },
      table: { schema: "public", table: "orders", page: 2, query },
      file: null,
    };
    const ws = useWorkspace.getState();
    seedEditor(id, text);
    ws.restore({ tabs: [saved], active: id, layout: ws.layout, notebooks: [], shown: null }, [connectionConfig("c1", "PostgreSQL")], ws.settings);
    await loadTableOnce(id);
    await vi.waitFor(() => expect(runs).toHaveLength(1));
    expect(runs).toEqual([{ tabId: id, request: { kind: "tablePage", schema: "public", table: "orders", page: 2, query }, confirmed: null }]);
    const tab = useWorkspace.getState().tabs[id];
    expect(tab && isBrowsing(tab)).toBe(true);
    expect(workspaceSnapshot(useWorkspace.getState(), { textOf, isDirty: () => false }).tabs).toEqual([saved]);
  },
);

it.each(["SELECT * FROM public.orders WHERE status = 'pending'", "SELECT * FROM public.orders;;", "SELECT * FROM public.orders; -- changed"])(
  "keeps a changed saved document %j out of table browsing",
  async (text) => {
    const runs: unknown[] = [];
    mockIPC((cmd, args) => {
      if (cmd === "script_params") return [];
      if (cmd === "table_script") return ordersScript;
      if (cmd === "console_run") runs.push(args);
      return null;
    });
    const id = crypto.randomUUID();
    seedEditor(id, text);
    useWorkspace.getState().addTab({
      id,
      title: "orders",
      console: { db: orders.db, mode: "AutoCommit" },
      table: { schema: "public", table: "orders", page: 2, query: { orderBy: "id DESC", filters: [], where: "" } },
      file: null,
    });
    await loadTableOnce(id);
    const tab = useWorkspace.getState().tabs[id];
    expect(tab && isBrowsing(tab)).toBe(false);
    expect(runs).toEqual([]);
    expect(textOf(id)).toBe(text);
  },
);

it("compares engine-supplied strings without interpreting their punctuation", async () => {
  const db = { connectionId: crypto.randomUUID(), database: "shop" };
  mockIPC((cmd) =>
    cmd === "table_script" ? ({ text: "engine template", acceptedTexts: ["engine template", "another engine form"] } satisfies TableScript) : null,
  );
  const id = crypto.randomUUID();
  seedEditor(id, "\n another engine form\t");
  useWorkspace.getState().addTab({
    id,
    title: "orders",
    console: { db, mode: "AutoCommit" },
    table: { schema: "public", table: "orders", page: 0, query: { orderBy: "", filters: [], where: "" } },
    file: null,
  });
  await browseTemplate(db, "public", "orders");
  const tab = useWorkspace.getState().tabs[id];
  expect(tab && isBrowsing(tab)).toBe(true);
  seedEditor(id, "engine template;");
  expect(tab && isBrowsing(tab)).toBe(false);
});

it("drops a table script reply after its dialect changes and loads the current dialect", async () => {
  const connectionId = crypto.randomUUID();
  const config = connectionConfig(connectionId, "dev");
  useWorkspace.getState().upsertConfig(config);
  const db = { connectionId, database: "shop" };
  let finish: (script: TableScript) => void = () => {
    throw new Error("table script was not requested");
  };
  mockIPC((cmd) =>
    cmd === "table_script"
      ? new Promise<TableScript>((resolve) => {
          finish = resolve;
        })
      : null,
  );
  const pending = browseTemplate(db, "public", "orders");
  useWorkspace.getState().upsertConfig({ ...config, databaseKind: "SqlServer" });
  finish(ordersScript);
  expect(await pending).toBeNull();
  mockIPC((cmd) =>
    cmd === "table_script" ? ({ text: "SELECT * FROM [public].[orders]", acceptedTexts: ["SELECT * FROM [public].[orders]"] } satisfies TableScript) : null,
  );
  await openTableTab({ db, schema: "public", relation: "orders" });
  const tab = useWorkspace.getState().tabs[activeId(useWorkspace.getState())!];
  expect(tab && textOf(tab.id)).toBe("SELECT * FROM [public].[orders]");
  expect(tab && isBrowsing(tab)).toBe(true);
});

it("drops a cached page load after its dialect changes while awaiting the template", async () => {
  const connectionId = crypto.randomUUID();
  const config = connectionConfig(connectionId, "dev");
  useWorkspace.getState().upsertConfig(config);
  const db = { connectionId, database: "shop" };
  await openTableTab({ db, schema: "public", relation: "orders" });
  const id = activeId(useWorkspace.getState())!;
  const calls: string[] = [];
  mockIPC((cmd) => {
    if (cmd === "script_params") return [];
    calls.push(cmd);
    return null;
  });
  const pending = loadTableOnce(id);
  useWorkspace.getState().upsertConfig({ ...config, databaseKind: "SqlServer" });
  await pending;
  expect(calls).toEqual([]);
});

it("never runs edited browse text as a script after awaiting console setup", async () => {
  await openTableTab(orders);
  const id = activeId(useWorkspace.getState())!;
  const calls: string[] = [];
  mockIPC((cmd) => {
    if (cmd === "script_params") return [];
    calls.push(cmd);
    if (cmd === "console_configure") insertAtCursor(id, "SELECT 42;\n");
    if (cmd === "console_run") return { runId: 1, statements: [] };
    return null;
  });
  await loadTableOnce(id);
  await vi.waitFor(() => expect(calls).toContain("console_configure"));
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(calls).toEqual(["console_configure"]);
  expect(textOf(id)).toBe("SELECT 42;\nSELECT * FROM public.orders");
});

it("drops formatting when the connection changes while the document stays the same", async () => {
  openScript("select 1", () => {
    moveCell("script", { connectionId: "sqlserver", database: "master" });
    return { from: 0, to: 8, insert: "SELECT 1" };
  });
  await formatActive();
  expect(textOf("script")).toBe("select 1");
});

it("passes SQL Server kind to text services before a catalog loads and reports unsupported formatting", async () => {
  useWorkspace.setState({ configs: { ms: { ...connectionConfig("ms", "SQL Server"), databaseKind: "SqlServer" } } });
  const sent = openScript("declare @x int = 1", () => Promise.reject({ kind: "Unsupported" }));
  moveCell("script", { connectionId: "ms", database: "master" });
  await formatActive();
  expect(sent).toEqual([{ text: "declare @x int = 1", target: { kind: "cursor", offset: 0 }, databaseKind: "SqlServer" }]);
  expect(useWorkspace.getState().toast?.text).toBe("Formatting is not supported for this SQL construct.");
});

it("does not run an outline response after switching the target with unchanged text", async () => {
  useWorkspace.setState({ configs: { ms: { ...connectionConfig("ms", "SQL Server"), databaseKind: "SqlServer" } } });
  const requests: unknown[] = [];
  const runs: unknown[] = [];
  const id = openSqlTab({ db: { connectionId: "ms", database: "master" }, text: "select 1" });
  mockIPC((cmd, args) => {
    if (cmd === "script_params") return [];
    if (cmd === "script_outline") {
      requests.push(args);
      moveCell(id, { connectionId: "ms", database: "other" });
      return { statements: [{ from: 0, to: 8 }], atCursor: 0, unitKind: "Batch" };
    }
    if (cmd === "console_run") runs.push(args);
    return null;
  });
  await runOrChoose();
  expect(requests).toEqual([{ text: "select 1", cursor: 0, databaseKind: "SqlServer" }]);
  expect(runs).toEqual([]);
});

it.each(["database", "mode"])("drops a pending run when its %s changes during console setup", async (change) => {
  const calls: string[] = [];
  let finish: () => void = () => {
    throw new Error("console setup was not requested");
  };
  const id = openSqlTab({ db: { connectionId: "c1", database: "shop" }, text: "SELECT 1" });
  mockIPC((cmd) => {
    if (cmd === "script_params") return [];
    calls.push(cmd);
    if (cmd === "console_configure")
      return new Promise<null>((resolve) => {
        finish = () => resolve(null);
      });
    if (cmd === "script_outline") return { statements: [{ from: 0, to: 8 }], atCursor: 0, unitKind: "Statement" };
    return null;
  });
  const pending = runOrChoose();
  await new Promise((resolve) => setTimeout(resolve, 0));
  if (change === "database") moveCell(id, { connectionId: "ms", database: "master" });
  else await setTabMode(id, "Manual");
  finish();
  await pending;
  expect(calls).toEqual(["console_configure"]);
});

describe("pages during an exact count", () => {
  beforeAll(() => installRunBehavior());

  function publish(id: string, patch: Partial<ConsoleState>) {
    const state = useLive.getState().consoles[id]!;
    useLive.getState().apply({ kind: "ConsoleChanged", ...state, ...patch, rev: state.rev + 1 });
  }

  async function pendingCount(databaseKind: DatabaseKind = "SqlServer") {
    useWorkspace.getState().upsertConfig({ ...connectionConfig("c1", "dev"), databaseKind });
    const id = await openWithConsole();
    publish(id, { session: { kind: "Open", backendPid: 1 }, run: { kind: "Finished", runId: 7, elapsedMs: 1, outcome: "Ok", statements: [] } });
    let finish: (count: number) => void = () => {
      throw new Error("count was not requested");
    };
    const requests: { cmd: string; args: unknown }[] = [];
    mockIPC((cmd, args) => {
      if (cmd === "script_params") return [];
      requests.push({ cmd, args });
      if (cmd === "exact_row_count")
        return new Promise<number>((resolve) => {
          finish = resolve;
        });
      if (cmd === "console_run") return { runId: 8, statements: [] };
      return null;
    });
    const pending = lastTablePage(id);
    return { id, pending, requests, finish };
  }

  it.each([false, true])("keeps only the latest page intent before the saved page changes, Busy published=%s", async (published) => {
    const { id, pending, requests, finish } = await pendingCount();
    if (published) publish(id, { work: "Count" });
    setTablePage(id, 1);
    setTablePage(id, 3);
    await Promise.resolve();
    expect(requests.map((r) => r.cmd)).toEqual(["exact_row_count"]);
    expect(useWorkspace.getState().tabs[id]?.table?.page).toBe(0);
    if (published) publish(id, { work: null });
    expect(requests.map((r) => r.cmd)).toEqual(["exact_row_count"]);
    finish(1251);
    await pending;
    await vi.waitFor(() => expect(requests.filter((r) => r.cmd === "console_run")).toHaveLength(1));
    expect(requests.at(-1)?.args).toMatchObject({ request: { kind: "tablePage", page: 3 } });
    await vi.waitFor(() => expect(useWorkspace.getState().tabs[id]?.table?.page).toBe(3));
    expect(useWorkspace.getState().tabs[id]?.table?.exactRowCount).toBeUndefined();
  });

  it("lets PostgreSQL load a newer page while its metadata count is pending", async () => {
    const { id, pending, requests, finish } = await pendingCount("Postgres");
    setTablePage(id, 1);
    await vi.waitFor(() => expect(useWorkspace.getState().tabs[id]?.table?.page).toBe(1));
    finish(1251);
    await pending;
    expect(requests.map((r) => r.cmd)).toEqual(["exact_row_count", "console_run"]);
    expect(requests[1]?.args).toMatchObject({ request: { kind: "tablePage", page: 1 } });
    expect(useWorkspace.getState().tabs[id]?.table?.page).toBe(1);
  });

  it("waits for the ownership snapshot when the count reply arrives first", async () => {
    const { id, pending, requests, finish } = await pendingCount();
    publish(id, { work: "Count" });
    finish(1251);
    await pending;
    expect(requests.map((r) => r.cmd)).toEqual(["exact_row_count"]);
    publish(id, { work: null });
    await vi.waitFor(() => expect(useWorkspace.getState().tabs[id]?.table?.page).toBe(2));
    expect(requests.at(-1)?.args).toMatchObject({ request: { kind: "tablePage", page: 2 } });
  });

  it("lets Escape discard a counted page queued before the clearing snapshot", async () => {
    const { id, pending, requests, finish } = await pendingCount();
    publish(id, { work: "Count" });
    finish(1251);
    await pending;
    expect(cancelActive()).toBe(true);
    publish(id, { work: null, session: { kind: "Lost", reason: "cancelled", transaction: "Unknown" } });
    publish(id, { work: null, session: { kind: "Open", backendPid: 2 } });
    await Promise.resolve();
    expect(requests.map((r) => r.cmd)).toEqual(["exact_row_count", "console_cancel"]);
    expect(useWorkspace.getState().tabs[id]?.table?.page).toBe(0);
  });

  it("queues a changed filter and drops the older Last result", async () => {
    const { id, pending, requests, finish } = await pendingCount();
    publish(id, { work: "Count" });
    updateTableQuery(id, (query) => ({ ...query, where: "id > 9" }));
    finish(1251);
    await pending;
    expect(requests.map((r) => r.cmd)).toEqual(["exact_row_count"]);
    publish(id, { work: null });
    await vi.waitFor(() => expect(requests).toHaveLength(2));
    expect(requests[1]?.args).toMatchObject({ request: { kind: "tablePage", page: 0, query: { where: "id > 9" } } });
  });

  it.each(["cancel", "close", "disconnect"])("does not run a queued page or repeat a count after %s", async (action) => {
    const { id, pending, requests, finish } = await pendingCount();
    publish(id, { work: "Count" });
    setTablePage(id, 1);
    if (action === "cancel") {
      expect(cancelActive()).toBe(true);
      expect(requests.map((r) => r.cmd)).toEqual(["exact_row_count", "console_cancel"]);
      publish(id, { work: null, session: { kind: "Lost", reason: "cancelled", transaction: "Unknown" } });
    } else if (action === "close") {
      useWorkspace.getState().closeTab(id);
      publish(id, { work: null, session: { kind: "None" } });
    } else {
      publish(id, { work: null, session: { kind: "None" } });
    }
    finish(1251);
    await pending;
    publish(id, { work: null, session: { kind: "Open", backendPid: 2 } });
    await Promise.resolve();
    expect(requests.filter((r) => r.cmd === "console_run")).toEqual([]);
    expect(requests.filter((r) => r.cmd === "exact_row_count")).toHaveLength(1);
  });

  it("keeps ordinary run page queuing and cancellation", async () => {
    const id = await openWithConsole();
    const requests: { cmd: string; args: unknown }[] = [];
    mockIPC((cmd, args) => {
      if (cmd === "script_params") return [];
      requests.push({ cmd, args });
      if (cmd === "console_run") return { runId: 8, statements: [] };
      return null;
    });
    publish(id, { work: "Run", session: { kind: "Open", backendPid: 1 }, run: { kind: "Running", runId: 7, startedAtMs: 1, statements: [] } });
    setTablePage(id, 1);
    setTablePage(id, 2);
    expect(requests).toEqual([]);
    publish(id, { work: null, run: { kind: "Finished", runId: 7, elapsedMs: 1, outcome: "Ok", statements: [] } });
    await vi.waitFor(() => expect(useWorkspace.getState().tabs[id]?.table?.page).toBe(2));
    expect(requests).toHaveLength(1);
    expect(requests[0]?.args).toMatchObject({ request: { kind: "tablePage", page: 2 } });
    publish(id, { work: "Run", run: { kind: "Running", runId: 8, startedAtMs: 1, statements: [] } });
    setTablePage(id, 3);
    expect(cancelActive()).toBe(true);
    publish(id, { work: null, run: { kind: "Finished", runId: 8, elapsedMs: 1, outcome: "Cancelled", statements: [] } });
    await Promise.resolve();
    expect(requests.map((r) => r.cmd)).toEqual(["console_run", "console_cancel"]);
  });

  it("does not allow Escape to cancel transaction, edit or probe work", async () => {
    const id = await openWithConsole();
    const requests: string[] = [];
    mockIPC((cmd) => {
      if (cmd === "script_params") return [];
      requests.push(cmd);
    });
    for (const work of ["Transaction", "Edit", "Probe"] as const) {
      publish(id, { work });
      expect(cancelActive()).toBe(false);
    }
    expect(requests).toEqual([]);
  });
});

describe("a run the engine asks about", () => {
  const SCRIPT = "update orders set n = 1;\nselect 1;\ndelete from orders;";
  const asked = (digest: string, patch: Partial<WriteConfirmation> = {}): RunError => ({
    kind: "ConfirmWrites",
    confirmation: {
      connection: "prod-postgres",
      database: "shop",
      unit: "Statement",
      sends: { count: 2, statements: ["update orders set n = 1", "delete from orders"] },
      holds: { count: 0, statements: [] },
      digest,
      onProduction: true,
      ...patch,
    },
  });

  function connection(patch: Partial<ConnectionConfig> = {}) {
    useWorkspace.setState({ configs: { prod: { ...connectionConfig("prod", "prod-postgres"), environment: "Production", ...patch } } });
  }

  function queueRunReplies(...replies: (RunError | null)[]) {
    const runs: { request?: unknown; confirmed?: unknown }[] = [];
    mockIPC((cmd, args) => {
      if (cmd === "script_params") return [];
      if (cmd === "script_outline") return { statements: [{ from: 0, to: 23 }], atCursor: 0, unitKind: "Statement" };
      if (cmd !== "console_run") return null;
      runs.push(args as { confirmed?: unknown });
      const reply = replies.shift();
      return reply ? Promise.reject(reply) : { runId: 1, statements: [] };
    });
    return runs;
  }

  const question = () => {
    const overlay = useWorkspace.getState().overlay;
    return overlay.kind === "ask" ? overlay : null;
  };

  it("shows the engine's question, then sends the same request with its digest", async () => {
    connection();
    const id = openSqlTab({ db: { connectionId: "prod", database: "shop" }, text: SCRIPT });
    const runs = queueRunReplies(asked("d1"));
    runActive({ kind: "all" });
    await vi.waitFor(() => expect(question()).not.toBeNull());
    expect(question()).toMatchObject({
      testId: "confirm",
      title: "Run 2 writes on Production?",
      body: "This run sends 2 statements that write to prod-postgres / shop.",
    });
    expect(question()?.choices).toEqual([{ id: "btn", label: "Run writes", variant: "danger" }]);
    expect(question()?.statements).toEqual(["update orders set n = 1", "delete from orders"]);
    expect(question()?.body).not.toContain("A rollback does not undo sequence values or outside side effects.");
    const request = { kind: "script", text: SCRIPT, target: { kind: "all" }, mode: "Run", params: [] };
    expect(runs).toEqual([{ tabId: id, request, confirmed: null }]);
    useWorkspace.getState().answer("btn");
    await vi.waitFor(() => expect(runs).toHaveLength(2));
    expect(runs[1]).toEqual({ tabId: id, request, confirmed: "d1" });
  });

  it("sends nothing more when ⌘↵'s question is cancelled", async () => {
    connection();
    openSqlTab({ db: { connectionId: "prod", database: "shop" }, text: SCRIPT });
    const runs = queueRunReplies(asked("d1", { sends: { count: 1, statements: ["update orders set n = 1"] } }));
    void runOrChoose();
    await vi.waitFor(() => expect(question()?.title).toBe("Run 1 write on Production?"));
    expect(runs).toMatchObject([{ request: { target: { kind: "cursor", offset: 0 } }, confirmed: null }]);
    useWorkspace.getState().close();
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(runs).toHaveLength(1);
    expect(useWorkspace.getState().overlay.kind).toBe("none");
  });

  it("says how many are shown when the engine sends the first three", async () => {
    connection();
    openSqlTab({ db: { connectionId: "prod", database: "shop" }, text: SCRIPT });
    queueRunReplies(asked("d1", { sends: { count: 5, statements: ["delete from a", "delete from b", "delete from c"] } }));
    runActive({ kind: "all" });
    await vi.waitFor(() => expect(question()?.body).toBe("This run sends 5 statements that write to prod-postgres / shop. 3 of 5 are shown."));
  });

  it("lists the writes the transaction holds when a run that writes nothing could commit them", async () => {
    connection();
    openSqlTab({ db: { connectionId: "prod", database: "shop" }, text: "select 1\ncommit" });
    const runs = queueRunReplies(
      asked("d1", { sends: { count: 0, statements: [] }, holds: { count: 2, statements: ["insert into t values (1)", "delete from t"] } }),
    );
    runActive({ kind: "all" });
    await vi.waitFor(() => expect(question()).not.toBeNull());
    expect(question()).toMatchObject({
      title: "Run on Production?",
      body: "2 writes to prod-postgres / shop ran on this tab before the connection was marked Production. If they are still in this tab's transaction, confirming covers them, so a later commit does not ask about them again.",
      statements: ["insert into t values (1)", "delete from t"],
    });
    expect(question()?.choices).toEqual([{ id: "btn", label: "Run", variant: "danger" }]);
    useWorkspace.getState().answer("btn");
    await vi.waitFor(() => expect(runs.map((r) => r.confirmed)).toEqual([null, "d1"]));
  });

  it("lists held writes before the run's own writes", async () => {
    connection();
    openSqlTab({ db: { connectionId: "prod", database: "shop" }, text: SCRIPT });
    queueRunReplies(asked("d1", { holds: { count: 4, statements: ["delete from a", "delete from b", "delete from c"] } }));
    runActive({ kind: "all" });
    await vi.waitFor(() => expect(question()).not.toBeNull());
    expect(question()).toMatchObject({
      title: "Run 2 writes on Production?",
      body: "This run sends 2 statements that write to prod-postgres / shop. 4 writes to prod-postgres / shop ran on this tab before the connection was marked Production. If they are still in this tab's transaction, confirming covers them, so a later commit does not ask about them again. 5 of 6 are shown.",
      statements: ["delete from a", "delete from b", "delete from c", "update orders set n = 1", "delete from orders"],
    });
  });

  it("names a SQL Server batch", async () => {
    connection({ name: "prod-mssql", databaseKind: "SqlServer" });
    openSqlTab({ db: { connectionId: "prod", database: "master" }, text: "EXEC dbo.purge" });
    queueRunReplies(asked("d1", { connection: "prod-mssql", database: "master", unit: "Batch", sends: { count: 1, statements: ["EXEC dbo.purge"] } }));
    runActive({ kind: "all" });
    await vi.waitFor(() => expect(question()?.body).toBe("This run sends 1 batch that writes to prod-mssql / master."));
  });

  it("asks again when the engine refuses the digest it gave", async () => {
    connection();
    openSqlTab({ db: { connectionId: "prod", database: "shop" }, text: SCRIPT });
    const runs = queueRunReplies(asked("d1"), asked("d2", { connection: "prod-renamed" }));
    runActive({ kind: "all" });
    await vi.waitFor(() => expect(question()).not.toBeNull());
    useWorkspace.getState().answer("btn");
    await vi.waitFor(() => expect(question()?.body).toBe("This run sends 2 statements that write to prod-renamed / shop."));
    useWorkspace.getState().answer("btn");
    await vi.waitFor(() => expect(runs).toHaveLength(3));
    expect(runs.map((r) => r.confirmed)).toEqual([null, "d1", "d2"]);
  });

  it("adds the rollback sentence when Explain Analyze asks, including when the run itself sends nothing", async () => {
    connection();
    const id = openSqlTab({ db: { connectionId: "prod", database: "shop" }, text: "update public.orders set n = 2" });
    const runs = queueRunReplies(asked("d1", { sends: { count: 0, statements: [] }, holds: { count: 1, statements: ["update public.orders set n = 1"] } }));
    explainActive("ExplainAnalyze");
    await vi.waitFor(() => expect(question()).not.toBeNull());
    expect(question()?.title).toBe("Run on Production?");
    expect(question()?.body).toContain("A rollback does not undo sequence values or outside side effects.");
    expect(runs[0]).toMatchObject({
      tabId: id,
      request: { kind: "script", text: "update public.orders set n = 2", mode: "ExplainAnalyze" },
      confirmed: null,
    });
  });

  it("sends Explain on SQL Server", async () => {
    connection({ databaseKind: "SqlServer" });
    const id = openSqlTab({ db: { connectionId: "prod", database: "shop" }, text: "select 1" });
    const runs = queueRunReplies(null);
    explainActive("Explain");
    await vi.waitFor(() => expect(runs).toHaveLength(1));
    expect(runs[0]).toMatchObject({ tabId: id, request: { kind: "script", mode: "Explain" }, confirmed: null });
    expect(question()).toBeNull();
  });

  it("asks before Explain Analyze runs a SQL Server batch off Production", async () => {
    useWorkspace.setState({ configs: { ms: { ...connectionConfig("ms", "dev-mssql"), databaseKind: "SqlServer" } } });
    openSqlTab({ db: { connectionId: "ms", database: "shop" }, text: "UPDATE dbo.orders SET status = status" });
    queueRunReplies(
      asked("d1", {
        connection: "dev-mssql",
        unit: "Batch",
        onProduction: false,
        sends: { count: 1, statements: ["UPDATE dbo.orders SET status = status"] },
        holds: { count: 1, statements: ["insert into t values (1)"] },
      }),
    );
    explainActive("ExplainAnalyze");
    await vi.waitFor(() => expect(question()).not.toBeNull());
    expect(question()).toMatchObject({
      title: "Run this batch?",
      body: "Explain Analyze executes this batch on dev-mssql / shop. This app does not roll it back. A write to dev-mssql / shop ran on this tab before the connection was marked Production. If it is still in this tab's transaction, confirming covers it, so a later commit does not ask about it again.",
    });
    expect(question()?.choices).toEqual([{ id: "btn", label: "Run", variant: "danger" }]);
    expect(question()?.body).not.toContain("A rollback does not undo sequence values or outside side effects.");
  });

  it("keeps the Production title for SQL Server Explain Analyze and omits the rollback sentence", async () => {
    connection({ name: "prod-mssql", databaseKind: "SqlServer" });
    openSqlTab({ db: { connectionId: "prod", database: "shop" }, text: "UPDATE dbo.orders SET status = status" });
    queueRunReplies(
      asked("d1", {
        connection: "prod-mssql",
        unit: "Batch",
        sends: { count: 1, statements: ["UPDATE dbo.orders SET status = status"] },
      }),
    );
    explainActive("ExplainAnalyze");
    await vi.waitFor(() => expect(question()).not.toBeNull());
    expect(question()?.title).toBe("Run 1 write on Production?");
    expect(question()?.body).toBe("This run sends 1 batch that writes to prod-mssql / shop.");
    expect(question()?.choices).toEqual([{ id: "btn", label: "Run writes", variant: "danger" }]);
  });

  it("sends nothing while the tab is browsing a table", async () => {
    const runs: unknown[] = [];
    mockIPC((cmd, args) => {
      if (cmd === "script_params") return [];
      if (cmd === "console_run") runs.push(args);
      if (cmd === "table_script") return ordersScript;
      return null;
    });
    connection();
    const id = crypto.randomUUID();
    seedEditor(id, ordersScript.text);
    useWorkspace.getState().addTab({
      id,
      title: "orders",
      console: { db: orders.db, mode: "AutoCommit" },
      table: { schema: "public", table: "orders", page: 0, query: { orderBy: "", filters: [], where: "" } },
      file: null,
    });
    await browseTemplate(orders.db, "public", "orders");
    expect(isBrowsing(useWorkspace.getState().tabs[id]!)).toBe(true);
    explainActive("Explain");
    expect(runs).toEqual([]);
  });

  it("runs once without asking when the engine accepts", async () => {
    connection();
    openSqlTab({ db: { connectionId: "prod", database: "shop" }, text: "select 1" });
    const runs = queueRunReplies(null);
    runActive({ kind: "all" });
    await vi.waitFor(() => expect(runs).toHaveLength(1));
    expect(runs[0]?.confirmed).toBeNull();
    expect(question()).toBeNull();
  });

  it("drops the run when the tab's target changes while it asks", async () => {
    connection();
    const id = openSqlTab({ db: { connectionId: "prod", database: "shop" }, text: SCRIPT });
    const runs = queueRunReplies(asked("d1"));
    runActive({ kind: "all" });
    await vi.waitFor(() => expect(question()).not.toBeNull());
    moveCell(id, { connectionId: "prod", database: "reporting" });
    useWorkspace.getState().answer("btn");
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(runs).toHaveLength(1);
  });
});

describe("staged edits the engine asks about", () => {
  const key = [{ column: "id", value: "1" }];
  const asked: ApplyError = {
    kind: "ConfirmWrites",
    confirmation: {
      connection: "prod-postgres",
      database: "shop",
      unit: "Statement",
      sends: { count: 2, statements: [] },
      holds: { count: 0, statements: [] },
      digest: "d1",
      onProduction: true,
    },
  };
  const question = () => {
    const overlay = useWorkspace.getState().overlay;
    return overlay.kind === "ask" ? overlay : null;
  };

  async function staged(...replies: (ApplyError | null)[]) {
    const id = await openWithConsole();
    useTableEdits.getState().edit(id, (edits) => markDeleted(setCell(edits, [{ column: "id", value: "2" }], "status", "x", "y"), [key], true));
    const applies: { confirmed?: unknown }[] = [];
    mockIPC((cmd, args) => {
      if (cmd === "script_params") return [];
      if (cmd !== "table_apply") return null;
      applies.push(args as { confirmed?: unknown });
      const reply = replies.shift();
      return reply ? Promise.reject(reply) : { committed: true };
    });
    return { id, applies };
  }

  it("shows the question, naming each kind of change and the table, then sends the digest", async () => {
    const { id, applies } = await staged(asked);
    const done = submitEdits(id);
    await vi.waitFor(() => expect(question()).not.toBeNull());
    expect(question()).toMatchObject({
      title: "Apply 2 changes on Production?",
      body: "This sends 1 update and 1 delete to public.orders on prod-postgres / shop.",
    });
    expect(applies.map((a) => a.confirmed)).toEqual([null]);
    useWorkspace.getState().answer("btn");
    await done;
    expect(applies.map((a) => a.confirmed)).toEqual([null, "d1"]);
    expect(useTableEdits.getState().byTab[id]).toBeUndefined();
  });

  it("names the writes the transaction already holds", async () => {
    const holds = { count: 1, statements: ["insert into t values (1)"] };
    const { id } = await staged({ ...asked, confirmation: { ...asked.confirmation, holds } });
    const done = submitEdits(id);
    await vi.waitFor(() => expect(question()).not.toBeNull());
    expect(question()).toMatchObject({
      body: "This sends 1 update and 1 delete to public.orders on prod-postgres / shop. A write to prod-postgres / shop ran on this tab before the connection was marked Production. If it is still in this tab's transaction, confirming covers it, so a later commit does not ask about it again.",
      statements: ["insert into t values (1)"],
    });
    useWorkspace.getState().close();
    await done;
  });

  it("keeps the edits when the question is cancelled", async () => {
    const { id, applies } = await staged(asked);
    const done = submitEdits(id);
    await vi.waitFor(() => expect(question()).not.toBeNull());
    useWorkspace.getState().close();
    await done;
    expect(applies).toHaveLength(1);
    expect(pendingCount(useTableEdits.getState().byTab[id]!)).toBe(2);
    expect(useTableEdits.getState().submitting[id]).toBeUndefined();
  });

  it("sends once without asking when the engine accepts", async () => {
    const { id, applies } = await staged(null);
    await submitEdits(id);
    expect(question()).toBeNull();
    expect(applies.map((a) => a.confirmed)).toEqual([null]);
  });

  it("asks before Revert drops staged edits", async () => {
    const { id } = await staged();
    const done = revertEdits(id);
    await vi.waitFor(() => expect(question()?.title).toBe("Revert 2 changes?"));
    useWorkspace.getState().close();
    await done;
    expect(pendingCount(useTableEdits.getState().byTab[id]!)).toBe(2);
    const again = revertEdits(id);
    await vi.waitFor(() => expect(question()).not.toBeNull());
    useWorkspace.getState().answer("btn");
    await again;
    expect(useTableEdits.getState().byTab[id]).toBeUndefined();
  });
});

describe("a commit the engine asks about", () => {
  const asked: TxnError = {
    kind: "ConfirmWrites",
    confirmation: {
      connection: "prod-postgres",
      database: "shop",
      unit: "Statement",
      sends: { count: 0, statements: [] },
      holds: { count: 1, statements: ["insert into t values (1)"] },
      digest: "c1",
      onProduction: true,
    },
  };

  function queueCommitReplies(...replies: (TxnError | null)[]) {
    const commits: unknown[] = [];
    mockIPC((cmd, args) => {
      if (cmd === "script_params") return [];
      if (cmd === "script_outline") return { statements: [], atCursor: null, unitKind: "Statement" };
      if (cmd !== "console_commit") return null;
      commits.push(args);
      const reply = replies.shift();
      return reply ? Promise.reject(reply) : null;
    });
    return commits;
  }

  const question = () => {
    const overlay = useWorkspace.getState().overlay;
    return overlay.kind === "ask" ? overlay : null;
  };

  it("shows the engine's question, then commits with its digest", async () => {
    const id = openSqlTab({ db: { connectionId: "prod", database: "shop" }, text: "" });
    const commits = queueCommitReplies(asked);
    const done = endTransaction(id, "commit");
    await vi.waitFor(() => expect(question()).not.toBeNull());
    expect(question()).toMatchObject({
      title: "Commit on Production?",
      body: "A write to prod-postgres / shop ran on this tab before the connection was marked Production. If it is still in this tab's transaction, this commit keeps it.",
      statements: ["insert into t values (1)"],
    });
    expect(question()?.choices).toEqual([{ id: "btn", label: "Commit writes", variant: "danger" }]);
    useWorkspace.getState().answer("btn");
    await done;
    expect(commits).toEqual([
      { tabId: id, confirmed: null },
      { tabId: id, confirmed: "c1" },
    ]);
  });

  it("commits nothing when the question is cancelled", async () => {
    const id = openSqlTab({ db: { connectionId: "prod", database: "shop" }, text: "" });
    const commits = queueCommitReplies(asked);
    const done = endTransaction(id, "commit");
    await vi.waitFor(() => expect(question()).not.toBeNull());
    useWorkspace.getState().close();
    await done;
    expect(commits).toEqual([{ tabId: id, confirmed: null }]);
  });
});

describe("parameter run intents", () => {
  it("reloads a browsing table when page size changes in Settings", async () => {
    const id = await openWithConsole();
    useWorkspace.getState().openSettings();
    const calls: { cmd: string; args: unknown }[] = [];
    mockIPC((cmd, args) => {
      calls.push({ cmd, args });
      if (cmd === "script_outline") return { statements: [], atCursor: null, unitKind: "Statement" };
      if (cmd === "console_run") return { runId: 1, statements: [] };
      return null;
    });
    await saveSettings({ tablePageSize: useWorkspace.getState().settings.tablePageSize + 50 });
    await vi.waitFor(() =>
      expect(calls).toContainEqual({
        cmd: "console_run",
        args: {
          tabId: id,
          request: { kind: "tablePage", schema: "public", table: "orders", page: 0, query: { orderBy: "", filters: [], where: "" } },
          confirmed: null,
        },
      }),
    );
    expect(calls.some(({ cmd }) => cmd === "script_params")).toBe(false);
    expect(useWorkspace.getState().screen.kind).toBe("settings");
  });

  it("keeps a canonical table page run when another tab becomes active", async () => {
    await openWithConsole();
    const calls: string[] = [];
    mockIPC((cmd) => {
      calls.push(cmd);
      if (cmd === "script_outline") return { statements: [], atCursor: null, unitKind: "Statement" };
      if (cmd === "console_run") return { runId: 1, statements: [] };
      return null;
    });
    runActive();
    openSqlTab();
    await vi.waitFor(() => expect(calls).toContain("console_run"));
    expect(calls).not.toContain("script_params");
  });

  it.each(["tab away and back", "cursor"])("drops a pending discovery after changing the %s", async (change) => {
    useWorkspace.setState({ configs: { c1: connectionConfig("c1", "dev") } });
    const db = { connectionId: "c1", database: "shop" };
    const id = openSqlTab({ db, text: "select 1 as first;\nselect 2 as second" });
    const other = openSqlTab({ db, text: "select 3" });
    useWorkspace.getState().setActive(id);
    const calls: string[] = [];
    let finish: () => void = () => {
      throw new Error("discovery was not requested");
    };
    mockIPC((cmd) => {
      calls.push(cmd);
      if (cmd === "script_outline")
        return {
          statements: [
            { from: 0, to: 18 },
            { from: 19, to: 37 },
          ],
          atCursor: 0,
          unitKind: "Statement",
        };
      if (cmd === "script_params")
        return new Promise<[]>((resolve) => {
          finish = () => resolve([]);
        });
      if (cmd === "console_run") return { runId: 1, statements: [] };
      return null;
    });
    const pending = runOrChoose();
    await vi.waitFor(() => expect(calls).toContain("script_params"));
    if (change === "cursor") moveCursor(id, 28);
    else {
      useWorkspace.getState().setActive(other);
      useWorkspace.getState().setActive(id);
    }
    finish();
    await pending;
    expect(calls).not.toContain("console_run");
    expect(useParams.getState().prompt).toBeNull();
  });

  function start(mode: "Run" | "Explain" = "Run") {
    useWorkspace.setState({ configs: { c1: connectionConfig("c1", "dev") } });
    const runs: unknown[] = [];
    mockIPC((cmd, args) => {
      if (cmd === "script_params") return [{ name: ":id" }];
      if (cmd === "script_outline") return { statements: [], atCursor: null, unitKind: "Statement" };
      if (cmd === "console_run") {
        runs.push(args);
        return { runId: 1, statements: [] };
      }
      return null;
    });
    const id = openSqlTab({ db: { connectionId: "c1", database: "shop" }, text: "select :id::integer" });
    if (mode === "Explain") explainActive("Explain");
    else runActive({ kind: "all" });
    return { id, runs };
  }

  it("uses the parameter snapshot for Explain and remembers it in this tab", async () => {
    const { id, runs } = start("Explain");
    await vi.waitFor(() => expect(useParams.getState().prompt).not.toBeNull());
    finishParams([{ name: ":id", value: "42" }]);
    await vi.waitFor(() => expect(runs).toHaveLength(1));
    expect(runs[0]).toMatchObject({ tabId: id, request: { kind: "script", mode: "Explain", params: [{ name: ":id", value: "42" }] } });
    runActive({ kind: "all" });
    await vi.waitFor(() => expect(useParams.getState().prompt?.values).toEqual([{ name: ":id", value: "42" }]));
    finishParams(null);
    expect(runs).toHaveLength(1);
  });

  it.each(["document", "tab", "database", "overlay"])("cancels a parameter prompt when its %s changes", async (change) => {
    const { id, runs } = start();
    await vi.waitFor(() => expect(useParams.getState().prompt).not.toBeNull());
    if (change === "document") seedEditor(id, "select 2");
    if (change === "tab") openSqlTab();
    if (change === "database") moveCell(id, { connectionId: "c1", database: "reporting" });
    if (change === "overlay") useWorkspace.getState().openSettings();
    expect(useParams.getState().prompt).toBeNull();
    await Promise.resolve();
    expect(runs).toEqual([]);
  });

  it("retries Production approval with the same parameter values and drops edited documents", async () => {
    const { id } = start();
    const runs: unknown[] = [];
    mockIPC((cmd, args) => {
      if (cmd === "script_params") return [{ name: ":id" }];
      if (cmd === "script_outline") return { statements: [], atCursor: null, unitKind: "Statement" };
      if (cmd !== "console_run") return null;
      runs.push(args);
      if (runs.length % 2 === 0) return { runId: runs.length, statements: [] };
      throw {
        kind: "ConfirmWrites",
        confirmation: {
          connection: "prod",
          database: "shop",
          unit: "Statement",
          sends: { count: 1, statements: ["update t set id = E'42'"] },
          holds: { count: 0, statements: [] },
          digest: "value-digest",
          onProduction: true,
        },
      } satisfies RunError;
    });
    await vi.waitFor(() => expect(useParams.getState().prompt).not.toBeNull());
    finishParams([{ name: ":id", value: "42" }]);
    await vi.waitFor(() => expect(useWorkspace.getState().overlay.kind).toBe("ask"));
    useWorkspace.getState().answer("btn");
    await vi.waitFor(() => expect(runs).toHaveLength(2));
    expect(runs[0]).toMatchObject({ confirmed: null, request: { params: [{ name: ":id", value: "42" }] } });
    expect(runs[1]).toMatchObject({ confirmed: "value-digest", request: { params: [{ name: ":id", value: "42" }] } });
    runActive({ kind: "all" });
    await vi.waitFor(() => expect(useParams.getState().prompt).not.toBeNull());
    finishParams([{ name: ":id", value: "42" }]);
    await vi.waitFor(() => expect(useWorkspace.getState().overlay.kind).toBe("ask"));
    seedEditor(id, "select changed");
    useWorkspace.getState().answer("btn");
    await Promise.resolve();
    expect(runs).toHaveLength(3);
  });

  it("supersedes a prompt and executes only the latest intent", async () => {
    const { runs } = start();
    await vi.waitFor(() => expect(useParams.getState().prompt).not.toBeNull());
    const first = useParams.getState().prompt;
    runActive({ kind: "all" });
    await vi.waitFor(() => expect(useParams.getState().prompt).not.toBe(first));
    await vi.waitFor(() => expect(useParams.getState().prompt).not.toBeNull());
    finishParams([{ name: ":id", value: "43" }]);
    await vi.waitFor(() => expect(runs).toHaveLength(1));
    expect(runs[0]).toMatchObject({ request: { params: [{ name: ":id", value: "43" }] } });
  });
});
