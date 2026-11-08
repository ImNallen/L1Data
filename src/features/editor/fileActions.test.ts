import { mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { FileBinding, FileOpened, FileReadError, FileWriteError, SaveTarget, Workspace } from "../../ipc/bindings";
import { activeId, allCells, useWorkspace } from "../../state/workspace";
import { docOf, dropEditor, insertAtCursor, isDirty, seedEditor, textOf } from "./editorStates";
import { closeTab } from "./closeTab";
import { defaultFileName, judgeFile, openFiles, reconcileFiles, saveTab, type FileRead } from "./fileActions";
import { forgetTab } from "./runActions";

const ws = () => useWorkspace.getState();
const PLAIN = { bom: false, crlf: false };

type Call = { cmd: string; args: Record<string, unknown> };
let calls: Call[];
let disk: Map<string, { text: string; stamp: string }>;
let dialogAnswers: unknown[];
let stamps: number;
let hold: { workspaceSave?: Promise<void>; fileSave?: Promise<void> };

const fileFor = (path: string): FileOpened => {
  const f = disk.get(path)!;
  return { binding: { path, stamp: f.stamp, format: PLAIN }, name: path.split("/").at(-1)!, text: f.text };
};
const writeDisk = (path: string, text: string) => disk.set(path, { text, stamp: `s${++stamps}` });
const fileSaves = () => calls.filter((c) => c.cmd === "file_save").map((c) => c.args as { target: SaveTarget; text: string });
const question = () => {
  const o = ws().overlay;
  return o.kind === "ask" ? o.title : null;
};

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
    if (cmd === "file_open") {
      const { path } = args as { path: string };
      if (!disk.has(path)) throw { kind: "NotFound" } satisfies FileReadError;
      return fileFor(path);
    }
    if (cmd === "file_save") {
      await hold.fileSave;
      const { target, text } = args as { target: SaveTarget; text: string };
      const path = target.kind === "Bound" ? target.binding.path : target.path;
      const now = disk.get(path);
      if (target.kind === "New" && target.taken.includes(path)) throw { kind: "OpenInAnotherTab", path } satisfies FileWriteError;
      if (target.kind === "Bound" && now && now.stamp !== target.binding.stamp && now.stamp !== target.overwrite) {
        throw { kind: "Changed", disk: now.stamp } satisfies FileWriteError;
      }
      writeDisk(path, text);
      const { binding, name } = fileFor(path);
      return { binding, name };
    }
    return null;
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
  for (const id of Object.keys(ws().tabs)) dropEditor(id);
  useWorkspace.setState(useWorkspace.getInitialState(), true);
});

async function openFile(path: string, text: string): Promise<string> {
  writeDisk(path, text);
  dialogAnswers.push([path]);
  await openFiles();
  return activeId(ws())!;
}

const binding = (stamp: string): FileBinding => ({ path: "/q/a.sql", stamp, format: PLAIN });
const read = (text: string, stamp: string): FileRead => ({ status: "ok", data: { binding: binding(stamp), name: "a.sql", text } });

it("judges a restored tab's fresh read by one table", () => {
  expect(judgeFile(binding("s1"), "select 1;", true, read("select 1;", "s2")).kind).toBe("adopt");
  expect(judgeFile(binding("s1"), "select 1; -- edited", true, read("select 1;", "s1"))).toEqual({ kind: "same", text: "select 1;" });
  expect(judgeFile(binding("s1"), "select 1;", false, read("select 2;", "s2")).kind).toBe("reload");
  expect(judgeFile(binding("s1"), "select 1; -- edited", true, read("select 2;", "s2")).kind).toBe("diverged");
  expect([false, true].map((dirty) => judgeFile(binding("s1"), "x", dirty, { status: "error", error: { kind: "NotFound" } }).kind)).toEqual([
    "missing",
    "missing",
  ]);
  expect(judgeFile(binding("s1"), "x", false, { status: "error", error: { kind: "NotUtf8", offset: 0 } }).kind).toBe("unreadable");
});

it("suggests a file name from the tab title", () => {
  expect(["Untitled-1.sql", "orders", "a/b: c", "q.psql", "v1.2 report"].map(defaultFileName)).toEqual([
    "Untitled-1.sql",
    "orders.sql",
    "a-b- c.sql",
    "q.psql",
    "v1.2 report.sql",
  ]);
});

it("opens each picked file in a clean tab named after it, and switches to a file that is already open", async () => {
  const a = await openFile("/q/a.sql", "select 1;");
  writeDisk("/q/b.sql", "select 2;");
  dialogAnswers.push(["/q/a.sql", "/q/b.sql"]);
  await openFiles();
  const b = activeId(ws())!;
  expect(allCells(ws())).toEqual([a, b]);
  expect([ws().tabs[b]?.title, textOf(b), isDirty(b), ws().tabs[b]?.file?.health]).toEqual(["b.sql", "select 2;", false, "ok"]);
  expect(calls.filter((c) => c.cmd === "file_open").map((c) => c.args.path)).toEqual(["/q/a.sql", "/q/b.sql"]);
});

it("toasts a file it cannot read and still opens the rest", async () => {
  writeDisk("/q/b.sql", "select 2;");
  dialogAnswers.push(["/q/gone.sql", "/q/b.sql"]);
  await openFiles();
  expect([ws().toast?.text, allCells(ws()).map((id) => ws().tabs[id]?.title)]).toEqual(["/q/gone.sql no longer exists.", ["b.sql"]]);
});

it("saves the workspace with the text before it writes that same text to the file", async () => {
  const id = await openFile("/q/a.sql", "select 1;");
  insertAtCursor(id, "-- edited ");
  calls = [];
  expect(await saveTab(id)).toBe(true);
  const order = calls.map((c) => c.cmd);
  expect(order).toEqual(["workspace_save", "file_save"]);
  const stored = (calls[0]!.args.workspace as Workspace).tabs[0]!.text;
  expect([stored, fileSaves()[0]!.text, disk.get("/q/a.sql")?.text, isDirty(id)]).toEqual([
    "-- edited select 1;",
    "-- edited select 1;",
    "-- edited select 1;",
    false,
  ]);
});

it("keeps a tab dirty when it was typed in while its save was out", async () => {
  const id = await openFile("/q/a.sql", "select 1;");
  insertAtCursor(id, "-- a ");
  let release = () => {};
  hold.fileSave = new Promise((r) => (release = r));
  const saving = saveTab(id);
  await vi.waitFor(() => expect(fileSaves()).toHaveLength(1));
  insertAtCursor(id, "-- b ");
  release();
  expect(await saving).toBe(true);
  expect([disk.get("/q/a.sql")?.text, isDirty(id)]).toEqual(["-- a select 1;", true]);
});

it("asks before overwriting an outside change, and overwrites only the version it showed, with the text it first sent", async () => {
  const id = await openFile("/q/a.sql", "select 1;");
  insertAtCursor(id, "-- mine ");
  writeDisk("/q/a.sql", "select 99;");
  const outside = disk.get("/q/a.sql")!.stamp;
  const saving = saveTab(id);
  await vi.waitFor(() => expect(question()).toBe("a.sql changed on disk"));
  insertAtCursor(id, "-- later ");
  ws().answer("overwrite");
  expect(await saving).toBe(true);
  const [first, retry] = fileSaves();
  expect(retry?.target).toMatchObject({ kind: "Bound", overwrite: outside });
  expect([first?.text, retry?.text, disk.get("/q/a.sql")?.text]).toEqual(["-- mine select 1;", "-- mine select 1;", "-- mine select 1;"]);
  expect(isDirty(id)).toBe(true);
});

it("asks again when the file changes once more while the overwrite question is up", async () => {
  const id = await openFile("/q/a.sql", "select 1;");
  writeDisk("/q/a.sql", "select 98;");
  const saving = saveTab(id);
  await vi.waitFor(() => expect(question()).toBe("a.sql changed on disk"));
  writeDisk("/q/a.sql", "select 99;");
  ws().answer("overwrite");
  await vi.waitFor(() => expect(fileSaves()).toHaveLength(2));
  await vi.waitFor(() => expect(question()).toBe("a.sql changed on disk"));
  ws().close();
  expect([await saving, disk.get("/q/a.sql")?.text, fileSaves().length]).toEqual([false, "select 99;", 2]);
});

it("writes nothing and stays dirty when the overwrite question is cancelled", async () => {
  const id = await openFile("/q/a.sql", "select 1;");
  insertAtCursor(id, "-- mine ");
  writeDisk("/q/a.sql", "select 99;");
  const saving = saveTab(id);
  await vi.waitFor(() => expect(question()).toBe("a.sql changed on disk"));
  ws().close();
  expect([await saving, fileSaves().length, disk.get("/q/a.sql")?.text, isDirty(id)]).toEqual([false, 1, "select 99;", true]);
});

it("reads unsaved once a save found an outside change it did not overwrite, so closing the tab asks", async () => {
  const id = await openFile("/q/a.sql", "select 1;");
  writeDisk("/q/a.sql", "select 99;");
  const saving = saveTab(id);
  await vi.waitFor(() => expect(question()).toBe("a.sql changed on disk"));
  ws().close();
  expect([await saving, isDirty(id), ws().tabs[id]?.file?.health, disk.get("/q/a.sql")?.text]).toEqual([false, true, "changed", "select 99;"]);
  void closeTab(id);
  await vi.waitFor(() => expect(question()).toBe("Save changes to a.sql?"));
  ws().answer("save");
  await vi.waitFor(() => expect(question()).toBe("a.sql changed on disk"));
  ws().answer("overwrite");
  await vi.waitFor(() => expect(ws().tabs[id]).toBeUndefined());
  expect(disk.get("/q/a.sql")?.text).toBe("select 1;");
});

it("writes nothing for a tab closed while its save waited on the workspace", async () => {
  const id = await openFile("/q/a.sql", "select 1;");
  insertAtCursor(id, "-- mine ");
  let release = () => {};
  hold.workspaceSave = new Promise((r) => (release = r));
  const saving = saveTab(id);
  await vi.waitFor(() => expect(calls.at(-1)?.cmd).toBe("workspace_save"));
  forgetTab(id);
  release();
  expect([await saving, fileSaves().length, disk.get("/q/a.sql")?.text]).toEqual([false, 0, "select 1;"]);
});

it("runs a tab's saves in order, so the second compares against the stamp the first wrote", async () => {
  const id = await openFile("/q/a.sql", "select 1;");
  insertAtCursor(id, "-- a ");
  const first = saveTab(id);
  const second = saveTab(id);
  expect([await first, await second]).toEqual([true, true]);
  const [one, two] = fileSaves();
  expect(two?.target.kind === "Bound" && one?.target.kind === "Bound" && two.target.binding.stamp !== one.target.binding.stamp).toBe(true);
  expect(question()).toBeNull();
});

it("saves a scratch tab through the save dialog and binds the tab to the new file", async () => {
  seedEditor("s", "select 3;");
  ws().addTab({ id: "s", title: "Untitled-1.sql", console: { db: null, mode: "AutoCommit" }, table: null, file: null });
  dialogAnswers.push("/q/new.sql");
  expect(await saveTab("s")).toBe(true);
  expect(calls.find((c) => c.cmd === "plugin:dialog|save")?.args.options).toMatchObject({ defaultPath: "Untitled-1.sql" });
  expect(fileSaves()[0]?.target).toEqual({ kind: "New", path: "/q/new.sql", format: null, taken: [] });
  expect([ws().tabs.s?.title, ws().tabs.s?.file?.binding.path, isDirty("s"), disk.get("/q/new.sql")?.text]).toEqual([
    "new.sql",
    "/q/new.sql",
    false,
    "select 3;",
  ]);
});

it("names the other tabs' files on Save As, so the engine refuses one of them, and keeps the tab's own file", async () => {
  await openFile("/q/a.sql", "select 1;");
  const b = await openFile("/q/b.sql", "select 2;");
  seedEditor("s", "");
  ws().addTab({ id: "s", title: "Untitled-1.sql", console: { db: null, mode: "AutoCommit" }, table: null, file: null });
  dialogAnswers.push("/q/a.sql");
  expect(await saveTab(b, "saveAs")).toBe(false);
  expect(fileSaves()[0]?.target).toEqual({ kind: "New", path: "/q/a.sql", format: PLAIN, taken: ["/q/a.sql"] });
  expect([ws().toast?.text, ws().tabs[b]?.file?.binding.path, disk.get("/q/a.sql")?.text]).toEqual([
    "/q/a.sql is open in another tab. Close that tab before you save over it.",
    "/q/b.sql",
    "select 1;",
  ]);
});

it("reloads a clean tab whose file changed, keeps a dirty one, and flags a missing file", async () => {
  const clean = await openFile("/q/a.sql", "select 1;");
  const edited = await openFile("/q/b.sql", "select 2;");
  const gone = await openFile("/q/c.sql", "select 3;");
  insertAtCursor(edited, "-- mine ");
  writeDisk("/q/a.sql", "select 10;");
  writeDisk("/q/b.sql", "select 20;");
  disk.delete("/q/c.sql");
  await reconcileFiles();
  const state = (id: string) => [textOf(id), isDirty(id), ws().tabs[id]?.file?.health];
  expect([state(clean), state(edited), state(gone)]).toEqual([
    ["select 10;", false, "ok"],
    ["-- mine select 2;", true, "changed"],
    ["select 3;", true, "missing"],
  ]);
  expect(ws().toast?.text).toBe("Reloaded a.sql from disk.");
  await reconcileFiles();
  expect([state(clean), state(edited), state(gone)].map((s) => s[2])).toEqual(["ok", "changed", "missing"]);
});

it("leaves a tab typed in while its file was being read", async () => {
  const id = await openFile("/q/a.sql", "select 1;");
  writeDisk("/q/a.sql", "select 10;");
  const reconciling = reconcileFiles();
  insertAtCursor(id, "-- typed ");
  await reconciling;
  expect([textOf(id), ws().tabs[id]?.file?.health, docOf(id) !== undefined]).toEqual(["-- typed select 1;", "ok", true]);
});
