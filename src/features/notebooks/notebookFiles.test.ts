import { mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type {
  CellMeta,
  ConnectionConfig,
  FileBinding,
  FileReadError,
  FileWriteError,
  NotebookCell,
  NotebookHeader,
  NotebookOpened,
  NotebookText,
  SaveTarget,
  Workspace,
} from "../../ipc/bindings";
import { judgeNotebook, savedCells, type NotebookRead } from "../../state/notebookFile";
import { activeId, DEFAULT_SETTINGS, shownNotebook, useWorkspace, workspaceSnapshot } from "../../state/workspace";
import { closeTab } from "../editor/closeTab";
import { dropEditor, insertAtCursor, seedEditor, textOf } from "../editor/editorStates";
import { forgetTab, openSqlTab } from "../editor/runActions";
import { notebookDirty } from "./notebookDirty";
import { defaultFileName, notebookDatabase, openNotebookFiles, reconcileNotebookFiles, saveNotebook } from "./notebookFiles";

const ws = () => useWorkspace.getState();
const PLAIN = { bom: false, crlf: false };
const NO_META: CellMeta = { title: null, schema: null, table: null, manual: false, unknown: [] };
const NO_HEADER: NotebookHeader = { connection: null, kind: null, database: null, unknown: [] };

type Call = { cmd: string; args: Record<string, unknown> };
let calls: Call[];
let disk: Map<string, { notebook: NotebookText; stamp: string }>;
let dialogAnswers: unknown[];
let stamps: number;
let hold: { workspaceSave?: Promise<void>; notebookSave?: Promise<void> };

const cell = (text: string, meta: Partial<CellMeta> = {}): NotebookCell => ({ text, meta: { ...NO_META, ...meta } });
const plainFile = (text: string): NotebookText => ({ plain: true, header: NO_HEADER, cells: [cell(text)] });
const opened = (path: string): NotebookOpened => {
  const f = disk.get(path)!;
  return { binding: { path, stamp: f.stamp, format: PLAIN }, name: path.split("/").at(-1)!, notebook: f.notebook };
};
const writeDisk = (path: string, notebook: NotebookText) => disk.set(path, { notebook, stamp: `s${++stamps}` });
const saves = () => calls.filter((c) => c.cmd === "notebook_save").map((c) => c.args as { target: SaveTarget; notebook: NotebookText });
const texts = (n: NotebookText | undefined) => n?.cells.map((c) => c.text);
const question = () => {
  const o = ws().overlay;
  return o.kind === "ask" ? o.title : null;
};
const shown = () => shownNotebook(ws())!;
const dirty = () => notebookDirty(shown().id);

const pg = (id: string, name: string, database = "shop"): ConnectionConfig => ({
  id,
  name,
  host: "localhost",
  port: 5433,
  database,
  user: "l1data",
  sslMode: "Prefer",
  caCertPath: null,
  databaseKind: "Postgres",
  hasPassword: true,
  color: null,
  ssh: null,
  environment: null,
  sqlServerAuth: null,
});
const mssql = (id: string, name: string): ConnectionConfig => ({ ...pg(id, name, "master"), databaseKind: "SqlServer", port: 1434 });

beforeEach(() => {
  vi.stubGlobal("window", { crypto });
  calls = [];
  disk = new Map();
  dialogAnswers = [];
  stamps = 0;
  hold = {};
  mockIPC(async (cmd, args) => {
    calls.push({ cmd, args: args as Record<string, unknown> });
    if (cmd === "plugin:dialog|open" || cmd === "plugin:dialog|save") return dialogAnswers.shift() ?? null;
    if (cmd === "workspace_save") return hold.workspaceSave;
    if (cmd === "notebook_open") {
      const { path } = args as { path: string };
      if (!disk.has(path)) throw { kind: "NotFound" } satisfies FileReadError;
      return opened(path);
    }
    if (cmd === "notebook_save") {
      await hold.notebookSave;
      const { target, notebook } = args as { target: SaveTarget; notebook: NotebookText };
      const path = target.kind === "Bound" ? target.binding.path : target.path;
      const now = disk.get(path);
      if (target.kind === "New" && target.taken.includes(path)) throw { kind: "OpenInAnotherNotebook", path } satisfies FileWriteError;
      if (target.kind === "Bound" && now && now.stamp !== target.binding.stamp && now.stamp !== target.overwrite) {
        throw { kind: "Changed", disk: now.stamp } satisfies FileWriteError;
      }
      const plain = notebook.plain && notebook.cells.length === 1 && JSON.stringify(notebook.cells[0]?.meta) === JSON.stringify(NO_META);
      writeDisk(path, { ...notebook, plain });
      const { binding, name } = opened(path);
      return { binding, name, plain };
    }
    return null;
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
  for (const id of Object.keys(ws().tabs)) dropEditor(id);
  useWorkspace.setState(useWorkspace.getInitialState(), true);
});

async function openFile(path: string, notebook: NotebookText): Promise<string> {
  writeDisk(path, notebook);
  dialogAnswers.push([path]);
  await openNotebookFiles();
  return shown().id;
}

const binding = (stamp: string): FileBinding => ({ path: "/q/a.sql", stamp, format: PLAIN });
const read = (text: string, stamp: string): NotebookRead => ({ status: "ok", data: { binding: binding(stamp), name: "a.sql", notebook: plainFile(text) } });

it("judges a restored notebook's fresh read by one table", () => {
  const mine = (text: string) => savedCells([cell(text)]);
  expect(judgeNotebook(binding("s1"), mine("select 1;"), true, read("select 1;", "s2")).kind).toBe("adopt");
  expect(judgeNotebook(binding("s1"), mine("select 1; -- edited"), true, read("select 1;", "s1")).kind).toBe("same");
  expect(judgeNotebook(binding("s1"), mine("select 1;"), false, read("select 2;", "s2")).kind).toBe("reload");
  expect(judgeNotebook(binding("s1"), mine("select 1; -- edited"), true, read("select 2;", "s2")).kind).toBe("diverged");
  expect(judgeNotebook(binding("s1"), mine("x"), false, { status: "error", error: { kind: "NotFound" } }).kind).toBe("missing");
  expect(judgeNotebook(binding("s1"), mine("x"), false, { status: "error", error: { kind: "NotUtf8", offset: 0 } }).kind).toBe("unreadable");
  const twoCells = savedCells([cell("select 1;"), cell("select 2;")]);
  expect(judgeNotebook(binding("s1"), twoCells, false, read("select 1;", "s1")).kind).toBe("same");
});

it("suggests a file name from the notebook title", () => {
  expect(["shop", "shop 2", "a/b: c", "q.psql", "v1.2 report"].map(defaultFileName)).toEqual([
    "shop.sql",
    "shop 2.sql",
    "a-b- c.sql",
    "q.psql",
    "v1.2 report.sql",
  ]);
});

describe("which database an opened file lands on", () => {
  const configs = { a: pg("a", "localhost / shop"), b: pg("b", "staging"), m: mssql("m", "warehouse") };
  const header = (h: Partial<NotebookHeader>): NotebookHeader => ({ ...NO_HEADER, ...h });
  const onShop = { connectionId: "b", database: "shop" };

  it("takes the connection the header names, with its kind, and the header's database", () => {
    const named = header({ connection: "localhost / shop", kind: "Postgres", database: "reporting" });
    expect(notebookDatabase(named, configs, onShop, {})).toEqual({ db: { connectionId: "a", database: "reporting" }, missing: null });
    expect(notebookDatabase(header({ connection: "staging" }), configs, null, {})).toEqual({ db: { connectionId: "b", database: "shop" }, missing: null });
    const wrongKind = header({ connection: "warehouse", kind: "Postgres" });
    expect(notebookDatabase(wrongKind, configs, onShop, {})).toEqual({ db: onShop, missing: "warehouse" });
  });

  it("opens a plain file on the shown database, and a named connection that is not here on the shown database of its kind", () => {
    expect(notebookDatabase(NO_HEADER, configs, { connectionId: "m", database: "master" }, {})).toEqual({
      db: { connectionId: "m", database: "master" },
      missing: null,
    });
    expect(notebookDatabase(NO_HEADER, configs, null, {})).toEqual({ db: null, missing: null });
    const gone = header({ connection: "prod", kind: "Postgres", database: "shop" });
    expect(notebookDatabase(gone, configs, onShop, {})).toEqual({ db: onShop, missing: "prod" });
  });

  it("falls back to the first connection of the header's kind, else to no connection", () => {
    const sqlServer = header({ connection: "prod", kind: "SqlServer", database: "sales" });
    expect(notebookDatabase(sqlServer, configs, onShop, {})).toEqual({ db: { connectionId: "m", database: "master" }, missing: "prod" });
    expect(notebookDatabase(sqlServer, { a: configs.a }, onShop, {})).toEqual({ db: null, missing: "prod" });
  });
});

it("opens each picked file as a notebook titled with its name, and shows the one already open for a file", async () => {
  const a = await openFile("/q/a.sql", plainFile("select 1;"));
  const twoCells: NotebookText = { plain: false, header: NO_HEADER, cells: [cell("select 2;", { title: "Totals" }), cell("select 3;", { manual: true })] };
  writeDisk("/q/b.sql", twoCells);
  dialogAnswers.push(["/q/a.sql", "/q/b.sql"]);
  await openNotebookFiles();
  const b = shown();
  expect(Object.values(ws().notebooks).map((n) => [n.title, n.file?.binding.path, n.file?.plain])).toEqual([
    ["a", "/q/a.sql", true],
    ["b", "/q/b.sql", false],
  ]);
  expect(b.cells.map((id) => [ws().tabs[id]?.title, ws().tabs[id]?.console.mode, textOf(id)])).toEqual([
    ["Totals", "AutoCommit", "select 2;"],
    ["Untitled-2.sql", "Manual", "select 3;"],
  ]);
  expect([notebookDirty(a), notebookDirty(b.id), activeId(ws())]).toEqual([false, false, b.cells[0]]);
  expect(calls.filter((c) => c.cmd === "notebook_open").map((c) => c.args.path)).toEqual(["/q/a.sql", "/q/b.sql"]);
  dialogAnswers.push(["/q/a.sql"]);
  await openNotebookFiles();
  expect([shown().id, Object.keys(ws().notebooks).length]).toEqual([a, 2]);
});

it("opens a file on the connection its header names, and toasts a connection it does not find", async () => {
  ws().upsertConfig(pg("c1", "local"));
  const local: NotebookText = {
    plain: false,
    header: { ...NO_HEADER, connection: "local", kind: "Postgres", database: "reporting" },
    cells: [cell("select 1;")],
  };
  await openFile("/q/local.sql", local);
  expect([shown().db, ws().toast]).toEqual([{ connectionId: "c1", database: "reporting" }, null]);
  await openFile("/q/prod.sql", { ...local, header: { ...local.header, connection: "prod" } });
  expect([shown().db, ws().toast?.text]).toEqual([{ connectionId: "c1", database: "reporting" }, "No connection named “prod”. Opened on reporting · local."]);
});

it("toasts a file it cannot read and still opens the rest", async () => {
  writeDisk("/q/b.sql", plainFile("select 2;"));
  dialogAnswers.push(["/q/gone.sql", "/q/b.sql"]);
  await openNotebookFiles();
  expect([ws().toast?.text, Object.values(ws().notebooks).map((n) => n.title)]).toEqual(["/q/gone.sql no longer exists.", ["b"]]);
});

describe("unsaved", () => {
  it("follows each cell's text and what the file records about it, and clears on save", async () => {
    const id = await openFile("/q/a.sql", { plain: false, header: NO_HEADER, cells: [cell("select 1;"), cell("select 2;")] });
    const [a, b] = shown().cells as [string, string];
    expect(dirty()).toBe(false);
    insertAtCursor(a, "-- edited ");
    expect(dirty()).toBe(true);
    insertAtCursor(a, "");
    expect(await saveNotebook(id)).toBe(true);
    expect([dirty(), texts(disk.get("/q/a.sql")?.notebook)]).toEqual([false, ["-- edited select 1;", "select 2;"]]);
    ws().patchConsole(b, { mode: "Manual" });
    expect(dirty()).toBe(true);
    ws().patchConsole(b, { mode: "AutoCommit" });
    expect(dirty()).toBe(false);
    ws().renameTab(b, "Untitled-9.sql");
    expect(dirty()).toBe(false);
    ws().renameTab(b, "Totals");
    expect(dirty()).toBe(true);
  });

  it("follows the cells added, closed, and moved since the last save", async () => {
    await openFile("/q/a.sql", { plain: false, header: NO_HEADER, cells: [cell("select 1;"), cell("select 2;")] });
    const [a, b] = shown().cells as [string, string];
    ws().moveCell(a, 1);
    expect([shown().cells, dirty()]).toEqual([[b, a], true]);
    ws().moveCell(a, 0);
    expect([shown().cells, dirty()]).toEqual([[a, b], false]);
    const c = openSqlTab();
    expect([shown().cells, dirty()]).toEqual([[a, b, c], true]);
    await closeTab(c);
    expect(dirty()).toBe(false);
    await closeTab(b);
    expect([question(), shown().cells, dirty()]).toEqual([null, [a], true]);
  });

  it("ignores a collapsed cell, which the workspace keeps across a restart and a reload and the file never records", async () => {
    const id = await openFile("/q/a.sql", { plain: false, header: NO_HEADER, cells: [cell("select 1;"), cell("select 2;")] });
    const [a, b] = shown().cells as [string, string];
    ws().setCollapsed([b], true);
    expect(dirty()).toBe(false);
    const saved: Workspace = JSON.parse(JSON.stringify(workspaceSnapshot(ws(), { textOf, isNotebookDirty: notebookDirty })));
    for (const t of Object.keys(ws().tabs)) dropEditor(t);
    useWorkspace.setState(useWorkspace.getInitialState(), true);
    for (const t of saved.tabs) seedEditor(t.id, t.text ?? "");
    ws().restore(saved, [], DEFAULT_SETTINGS);
    const collapsed = () => [a, b].map((c) => ws().tabs[c]?.collapsed ?? false);
    expect([collapsed(), dirty()]).toEqual([[false, true], false]);
    writeDisk("/q/a.sql", { plain: false, header: NO_HEADER, cells: [cell("select 1;"), cell("select 3;")] });
    await reconcileNotebookFiles();
    expect([shown().cells.map(textOf), collapsed(), dirty()]).toEqual([["select 1;", "select 3;"], [false, true], false]);
    insertAtCursor(a, "-- edited ");
    expect(await saveNotebook(id)).toBe(true);
    expect(saves()[0]?.notebook.cells.map((c) => c.meta)).toEqual([NO_META, NO_META]);
  });

  it("writes the cells in their moved order, and reads saved after", async () => {
    const id = await openFile("/q/a.sql", { plain: false, header: NO_HEADER, cells: [cell("select 1;"), cell("select 2;"), cell("select 3;")] });
    ws().moveCell(shown().cells[2]!, 0);
    expect(await saveNotebook(id)).toBe(true);
    expect([dirty(), texts(disk.get("/q/a.sql")?.notebook)]).toEqual([false, ["select 3;", "select 1;", "select 2;"]]);
  });

  it("is unknown for a notebook restored unsaved, until boot reads the file", async () => {
    await openFile("/q/a.sql", plainFile("select 1;"));
    insertAtCursor(shown().cells[0]!, "-- mine ");
    const saved = workspaceSnapshot(ws(), { textOf, isNotebookDirty: notebookDirty });
    for (const id of Object.keys(ws().tabs)) dropEditor(id);
    useWorkspace.setState(useWorkspace.getInitialState(), true);
    for (const t of saved.tabs) seedEditor(t.id, t.text ?? "");
    ws().restore(saved, [], DEFAULT_SETTINGS);
    expect([shown().file?.saved, dirty()]).toEqual([null, true]);
    await reconcileNotebookFiles();
    expect([shown().file?.saved, dirty()]).toEqual([[{ text: "select 1;", meta: savedCells([cell("")])[0]!.meta }], true]);
  });
});

it("saves the workspace with the cells before it writes those same cells to the file", async () => {
  const id = await openFile("/q/a.sql", plainFile("select 1;"));
  insertAtCursor(shown().cells[0]!, "-- edited ");
  calls = [];
  expect(await saveNotebook(id)).toBe(true);
  expect(calls.map((c) => c.cmd)).toEqual(["workspace_save", "notebook_save"]);
  const stored = (calls[0]!.args.workspace as Workspace).tabs[0]!.text;
  expect([stored, texts(saves()[0]?.notebook), saves()[0]?.notebook.plain, disk.get("/q/a.sql")?.notebook.plain]).toEqual([
    "-- edited select 1;",
    ["-- edited select 1;"],
    true,
    true,
  ]);
});

it("keeps a notebook unsaved when it was typed in while its save was out", async () => {
  const id = await openFile("/q/a.sql", plainFile("select 1;"));
  const cellId = shown().cells[0]!;
  insertAtCursor(cellId, "-- a ");
  let release = () => {};
  hold.notebookSave = new Promise((r) => (release = r));
  const saving = saveNotebook(id);
  await vi.waitFor(() => expect(saves()).toHaveLength(1));
  insertAtCursor(cellId, "-- b ");
  release();
  expect(await saving).toBe(true);
  expect([texts(disk.get("/q/a.sql")?.notebook), dirty()]).toEqual([["-- a select 1;"], true]);
});

it("asks before overwriting an outside change, and overwrites only the version it showed, with the cells it first sent", async () => {
  const id = await openFile("/q/a.sql", plainFile("select 1;"));
  const cellId = shown().cells[0]!;
  insertAtCursor(cellId, "-- mine ");
  writeDisk("/q/a.sql", plainFile("select 99;"));
  const outside = disk.get("/q/a.sql")!.stamp;
  const saving = saveNotebook(id);
  await vi.waitFor(() => expect(question()).toBe("a changed on disk"));
  insertAtCursor(cellId, "-- later ");
  ws().answer("overwrite");
  expect(await saving).toBe(true);
  const [first, retry] = saves();
  expect(retry?.target).toMatchObject({ kind: "Bound", overwrite: outside });
  expect([texts(first?.notebook), texts(retry?.notebook), texts(disk.get("/q/a.sql")?.notebook)]).toEqual([
    ["-- mine select 1;"],
    ["-- mine select 1;"],
    ["-- mine select 1;"],
  ]);
  expect(dirty()).toBe(true);
});

it("writes nothing and reads unsaved once an outside change it did not overwrite is found, so deleting it asks", async () => {
  const id = await openFile("/q/a.sql", plainFile("select 1;"));
  writeDisk("/q/a.sql", plainFile("select 99;"));
  const saving = saveNotebook(id);
  await vi.waitFor(() => expect(question()).toBe("a changed on disk"));
  ws().close();
  expect([await saving, saves().length, dirty(), shown().file?.health, texts(disk.get("/q/a.sql")?.notebook)]).toEqual([
    false,
    1,
    true,
    "changed",
    ["select 99;"],
  ]);
  void closeTab(shown().cells[0]!);
  await vi.waitFor(() => expect(question()).toBe("Save changes to a?"));
  expect(ws().overlay).toMatchObject({ body: "The file changed on disk. Don't Save keeps the file as it is now and drops this notebook's changes." });
  ws().answer("save");
  await vi.waitFor(() => expect(question()).toBe("a changed on disk"));
  ws().answer("overwrite");
  await vi.waitFor(() => expect(Object.keys(ws().notebooks)).toEqual([]));
  expect(texts(disk.get("/q/a.sql")?.notebook)).toEqual(["select 1;"]);
});

it("writes nothing for a notebook closed while its save waited on the workspace", async () => {
  const id = await openFile("/q/a.sql", plainFile("select 1;"));
  const cellId = shown().cells[0]!;
  insertAtCursor(cellId, "-- mine ");
  let release = () => {};
  hold.workspaceSave = new Promise((r) => (release = r));
  const saving = saveNotebook(id);
  await vi.waitFor(() => expect(calls.at(-1)?.cmd).toBe("workspace_save"));
  forgetTab(cellId);
  release();
  expect([await saving, saves().length, texts(disk.get("/q/a.sql")?.notebook)]).toEqual([false, 0, ["select 1;"]]);
});

it("runs a notebook's saves in order, so the second compares against the stamp the first wrote", async () => {
  const id = await openFile("/q/a.sql", plainFile("select 1;"));
  insertAtCursor(shown().cells[0]!, "-- a ");
  const first = saveNotebook(id);
  const second = saveNotebook(id);
  expect([await first, await second]).toEqual([true, true]);
  const [one, two] = saves();
  expect(two?.target.kind === "Bound" && one?.target.kind === "Bound" && two.target.binding.stamp !== one.target.binding.stamp).toBe(true);
  expect(question()).toBeNull();
});

it("saves a new notebook through the save dialog, names the file after it, and takes the file's name when it had its default title", async () => {
  ws().upsertConfig(pg("c1", "local"));
  openSqlTab({ db: { connectionId: "c1", database: "shop" }, text: "select 3;" });
  openSqlTab({ text: "select 4;" });
  dialogAnswers.push("/q/revenue.sql");
  expect(await saveNotebook(shown().id)).toBe(true);
  expect(calls.find((c) => c.cmd === "plugin:dialog|save")?.args.options).toMatchObject({ defaultPath: "shop.sql" });
  expect(saves()[0]?.target).toEqual({ kind: "New", path: "/q/revenue.sql", format: null, taken: [] });
  expect(saves()[0]?.notebook).toMatchObject({ plain: false, header: { connection: "local", kind: "Postgres", database: "shop", unknown: [] } });
  expect([shown().title, shown().file?.binding.path, shown().file?.plain, dirty()]).toEqual(["revenue", "/q/revenue.sql", false, false]);
  ws().renameNotebook(shown().id, "Quarterly");
  dialogAnswers.push("/q/q3.sql");
  expect(await saveNotebook(shown().id, "saveAs")).toBe(true);
  expect(calls.filter((c) => c.cmd === "plugin:dialog|save").at(-1)?.args.options).toMatchObject({ defaultPath: "/q/revenue.sql" });
  expect([shown().title, shown().file?.binding.path]).toEqual(["Quarterly", "/q/q3.sql"]);
});

it("names the other notebooks' files on Save As, so the engine refuses one of them, and keeps the notebook's own file", async () => {
  await openFile("/q/a.sql", plainFile("select 1;"));
  const b = await openFile("/q/b.sql", plainFile("select 2;"));
  dialogAnswers.push("/q/a.sql");
  expect(await saveNotebook(b, "saveAs")).toBe(false);
  expect(saves()[0]?.target).toEqual({ kind: "New", path: "/q/a.sql", format: PLAIN, taken: ["/q/a.sql"] });
  expect([ws().toast?.text, ws().notebooks[b]?.file?.binding.path, texts(disk.get("/q/a.sql")?.notebook)]).toEqual([
    "/q/a.sql is open in another notebook. Delete that notebook before you save over its file.",
    "/q/b.sql",
    ["select 1;"],
  ]);
});

it("keeps a plain file plain while it is one bare cell, and writes the header and markers once it is not", async () => {
  const id = await openFile("/q/a.sql", plainFile("select 1;"));
  insertAtCursor(shown().cells[0]!, "-- one ");
  await saveNotebook(id);
  expect([disk.get("/q/a.sql")?.notebook.plain, shown().file?.plain]).toEqual([true, true]);
  openSqlTab({ text: "select 2;" });
  await saveNotebook(id);
  expect([disk.get("/q/a.sql")?.notebook.plain, shown().file?.plain, texts(saves().at(-1)?.notebook)]).toEqual([
    false,
    false,
    ["-- one select 1;", "select 2;"],
  ]);
});

it("reloads a saved notebook whose file changed, keeps an unsaved one, and flags a missing file", async () => {
  const clean = await openFile("/q/a.sql", plainFile("select 1;"));
  const edited = await openFile("/q/b.sql", plainFile("select 2;"));
  const gone = await openFile("/q/c.sql", plainFile("select 3;"));
  const cellsOf = (id: string) => ws().notebooks[id]!.cells;
  insertAtCursor(cellsOf(edited)[0]!, "-- mine ");
  const kept = cellsOf(clean)[0];
  writeDisk("/q/a.sql", { plain: false, header: NO_HEADER, cells: [cell("select 10;"), cell("select 11;", { title: "More" })] });
  writeDisk("/q/b.sql", plainFile("select 20;"));
  disk.delete("/q/c.sql");
  await reconcileNotebookFiles();
  const state = (id: string) => [ws().notebooks[id]!.cells.map(textOf), notebookDirty(id), ws().notebooks[id]?.file?.health];
  expect([state(clean), state(edited), state(gone)]).toEqual([
    [["select 10;", "select 11;"], false, "ok"],
    [["-- mine select 2;"], true, "changed"],
    [["select 3;"], true, "missing"],
  ]);
  expect([cellsOf(clean)[0], ws().tabs[cellsOf(clean)[1]!]?.title, ws().notebooks[clean]?.file?.plain]).toEqual([kept, "More", false]);
  expect(ws().toast?.text).toBe("Reloaded a.sql from disk.");
  await reconcileNotebookFiles();
  expect([state(clean), state(edited), state(gone)].map((s) => s[2])).toEqual(["ok", "changed", "missing"]);
});

it("leaves a notebook typed in while its file was being read", async () => {
  const id = await openFile("/q/a.sql", plainFile("select 1;"));
  const cellId = shown().cells[0]!;
  writeDisk("/q/a.sql", plainFile("select 10;"));
  const reconciling = reconcileNotebookFiles();
  insertAtCursor(cellId, "-- typed ");
  await reconciling;
  expect([textOf(cellId), ws().notebooks[id]?.file?.health]).toEqual(["-- typed select 1;", "ok"]);
});

it("keeps a file notebook bound to its file when a new connection gives its cells a database", async () => {
  const id = await openFile("/q/a.sql", plainFile("select 1;"));
  expect(ws().notebooks[id]?.db).toBeNull();
  ws().upsertConfig(pg("c1", "local"));
  const after = ws().notebooks[id];
  expect([after?.db, after?.title, after?.file?.binding.path, notebookDirty(id)]).toEqual([{ connectionId: "c1", database: "shop" }, "a", "/q/a.sql", false]);
});

it("takes the keys another program changed when it keeps a file whose cells did not change", async () => {
  const header = (owner: string): NotebookHeader => ({ ...NO_HEADER, unknown: [{ key: "owner", value: owner }] });
  const id = await openFile("/q/a.sql", { plain: false, header: header("ana"), cells: [cell("select 1;", { unknown: [{ key: "tag", value: "old" }] })] });
  writeDisk("/q/a.sql", { plain: false, header: header("bo"), cells: [cell("select 1;", { unknown: [{ key: "tag", value: "new" }] })] });
  await reconcileNotebookFiles();
  insertAtCursor(ws().notebooks[id]!.cells[0]!, "-- edit ");
  await saveNotebook(id);
  const written = saves().at(-1)?.notebook;
  expect([written?.header.unknown, written?.cells[0]?.meta.unknown]).toEqual([[{ key: "owner", value: "bo" }], [{ key: "tag", value: "new" }]]);
});
