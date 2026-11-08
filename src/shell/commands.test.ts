import { mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, expect, it, vi } from "vitest";
import { dropEditor, seedEditor } from "../features/editor/editorStates";
import { setCell, useTableEdits } from "../state/tableEdits";
import { allCells, ask, EMPTY_QUERY, shownNotebook, useWorkspace } from "../state/workspace";
import { COMMANDS, START_COMMANDS, commandForKey, commandLabel, handleGlobalKey, openCommandPalette } from "./commands";

type Mods = Partial<Record<"metaKey" | "ctrlKey" | "shiftKey" | "altKey", boolean>>;
const chord = (key: string, mods: Mods) => ({ key, metaKey: false, ctrlKey: false, shiftKey: false, altKey: false, ...mods });
const press = (key: string, mods: Mods = { metaKey: true }) => commandForKey(chord(key, mods), true);
const pressOff = (key: string, mods: Mods) => commandForKey(chord(key, mods), false);

afterEach(() => {
  vi.unstubAllGlobals();
  for (const id of Object.keys(useWorkspace.getState().tabs)) dropEditor(id);
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useTableEdits.setState(useTableEdits.getInitialState(), true);
});

function recordIpc(): string[] {
  const cmds: string[] = [];
  vi.stubGlobal("window", { crypto });
  mockIPC((cmd) => {
    cmds.push(cmd);
    return cmd === "table_apply" ? { committed: true } : null;
  });
  return cmds;
}

it("asks for a path on ⌘⇧S even with staged edits, while ⌘S submits them and then saves", async () => {
  const cmds = recordIpc();
  seedEditor("orders", "SELECT * FROM public.orders");
  useWorkspace.getState().addTab({
    id: "orders",
    title: "orders",
    console: { db: { connectionId: "c", database: "shop" }, mode: "AutoCommit" },
    table: { schema: "public", table: "orders", page: 0, query: EMPTY_QUERY },
    file: null,
  });
  useTableEdits.getState().edit("orders", (e) => setCell(e, [{ column: "id", value: "1" }], "total", "9", "1"));
  const saveAs = keydown("s", { ctrlKey: true, shiftKey: true }, false);
  await vi.waitFor(() => expect(cmds).toEqual(["plugin:dialog|save"]));
  expect(saveAs.prevented).toBe(true);
  keydown("s", { ctrlKey: true }, false);
  await vi.waitFor(() => expect(cmds).toContain("table_apply"));
  expect(cmds.filter((c) => c === "plugin:dialog|save")).toHaveLength(1);
  keydown("s", { ctrlKey: true }, false);
  await vi.waitFor(() => expect(cmds.filter((c) => c === "plugin:dialog|save")).toHaveLength(2));
});

it("opens no file dialog while a question is up, and leaves the question for its answer", async () => {
  const cmds = recordIpc();
  const answer = ask({ title: "Overwrite?", body: "", choices: [{ id: "overwrite", label: "Overwrite", variant: "danger" }] });
  press("o")?.run();
  await new Promise((r) => setTimeout(r, 0));
  expect([cmds, useWorkspace.getState().overlay.kind]).toEqual([[], "ask"]);
  useWorkspace.getState().answer("overwrite");
  expect(await answer).toBe("overwrite");
  useWorkspace.getState().openSettings();
  press("o")?.run();
  await vi.waitFor(() => expect(cmds).toEqual(["plugin:dialog|open"]));
  expect(useWorkspace.getState().screen).toEqual({ kind: "workspace" });
});

it("does not save while a dialog is up", async () => {
  const cmds = recordIpc();
  seedEditor("t1", "select 1;");
  useWorkspace.getState().addTab({ id: "t1", title: "a.sql", console: { db: null, mode: "AutoCommit" }, table: null, file: null });
  useWorkspace.getState().open({ kind: "history" });
  press("s")?.run();
  press("s", { metaKey: true, shiftKey: true })?.run();
  press("e", { metaKey: true, shiftKey: true })?.run();
  await new Promise((r) => setTimeout(r, 0));
  expect(cmds).toEqual([]);
});

it("maps each shortcut to its command", () => {
  expect(["t", "n", "p", ",", "w", "e"].map((k) => press(k)?.id)).toEqual([
    "new-query",
    "new-connection",
    "navigation",
    "settings",
    "close-tab",
    "saved-queries",
  ]);
  expect(press("T")?.id).toBe("new-query");
  expect([press("e", { metaKey: true, shiftKey: true })?.id, pressOff("E", { ctrlKey: true, shiftKey: true })?.id]).toEqual(["save-query", "save-query"]);
});

it("toggles Saved queries on its shortcut, leaving settings for the workspace first", () => {
  useWorkspace.getState().openSettings("general");
  press("e")?.run();
  expect([useWorkspace.getState().screen, useWorkspace.getState().overlay]).toEqual([{ kind: "workspace" }, { kind: "saved", entry: { kind: "browse" } }]);
  press("e")?.run();
  expect(useWorkspace.getState().overlay).toEqual({ kind: "none" });
});

it("saves the active tab's statement on ⌘⇧E", async () => {
  vi.stubGlobal("window", { crypto });
  mockIPC((cmd) => (cmd === "saved_query_save" ? { id: 1, alreadySaved: false, queries: { rev: 2, items: [] } } : null));
  seedEditor("t1", "select 1;");
  useWorkspace.getState().addTab({ id: "t1", title: "a.sql", console: { db: null, mode: "AutoCommit" }, table: null, file: null });
  press("e", { metaKey: true, shiftKey: true })?.run();
  await vi.waitFor(() => expect(useWorkspace.getState().overlay).toEqual({ kind: "saved", entry: { kind: "name", id: 1 } }));
});

it("takes Cmd alone on macOS and leaves the Ctrl chords to the editor", () => {
  expect([press("t", { ctrlKey: true }), press("p", { ctrlKey: true }), press("t", { metaKey: true, ctrlKey: true })]).toEqual([
    undefined,
    undefined,
    undefined,
  ]);
});

it("takes Ctrl alone off macOS", () => {
  expect([pressOff("t", { ctrlKey: true })?.id, pressOff("w", { ctrlKey: true })?.id]).toEqual(["new-query", "close-tab"]);
  expect([pressOff("t", { metaKey: true }), pressOff("t", { metaKey: true, ctrlKey: true })]).toEqual([undefined, undefined]);
});

function keydown(key: string, mods: Mods, defaultPrevented: boolean) {
  const e = { ...chord(key, mods), defaultPrevented, prevented: false, preventDefault: () => (e.prevented = true) };
  handleGlobalKey(e as unknown as KeyboardEvent);
  return e;
}

it("leaves a chord a focused control already handled, and claims the ones it runs", () => {
  const handled = keydown("n", { ctrlKey: true }, true);
  expect([useWorkspace.getState().overlay, handled.prevented]).toEqual([{ kind: "none" }, false]);
  const ran = keydown("n", { ctrlKey: true }, false);
  expect([useWorkspace.getState().overlay, ran.prevented]).toEqual([{ kind: "connection", editing: null }, true]);
});

it("maps the file shortcuts, with Shift telling Save As from Save", () => {
  expect([press("o")?.id, press("s")?.id, press("s", { metaKey: true, shiftKey: true })?.id, press("S", { metaKey: true, shiftKey: true })?.id]).toEqual([
    "open-file",
    "save",
    "save-as",
    "save-as",
  ]);
  expect(press("s", { metaKey: true, altKey: true })).toBeUndefined();
});

it("ignores plain keys and shifted or alt chords", () => {
  expect(press("t", {})).toBeUndefined();
  expect(press("t", { metaKey: true, shiftKey: true })).toBeUndefined();
  expect(press("p", { metaKey: true, altKey: true })).toBeUndefined();
  expect(press("k")).toBeUndefined();
});

it("opens the overlay a command names", () => {
  press("n")?.run();
  expect(useWorkspace.getState().overlay).toEqual({ kind: "connection", editing: null });
});

const searchState = () => {
  const { screen, overlay, layout, treeQuery } = useWorkspace.getState();
  return { screen, overlay, sidebar: layout.sidebar, treeQuery };
};

it("opens quick navigation on ⌘P, replacing the current overlay and preserving the tree filter", () => {
  useWorkspace.getState().open({ kind: "history" });
  useWorkspace.getState().setTreeQuery("orders");
  press("p")?.run();
  expect(searchState()).toEqual({ screen: { kind: "workspace" }, overlay: { kind: "navigation" }, sidebar: true, treeQuery: "orders" });
});

it("keeps a hidden sidebar hidden on ⌘P", () => {
  useWorkspace.getState().setLayout({ sidebar: false });
  press("p")?.run();
  expect(searchState()).toEqual({ screen: { kind: "workspace" }, overlay: { kind: "navigation" }, sidebar: false, treeQuery: "" });
});

it("opens quick navigation above Settings and preserves its filter", () => {
  useWorkspace.getState().setLayout({ sidebar: false });
  useWorkspace.getState().openSettings("general");
  useWorkspace.getState().setSettingsQuery("row");
  press("p")?.run();
  expect(searchState()).toEqual({
    screen: { kind: "settings", section: "general", query: "row" },
    overlay: { kind: "navigation" },
    sidebar: false,
    treeQuery: "",
  });
});

it("toggles the settings screen on its shortcut, starting at General", () => {
  press(",")?.run();
  expect(useWorkspace.getState().screen).toEqual({ kind: "settings", section: "general", query: "" });
  press(",")?.run();
  expect(useWorkspace.getState().screen).toEqual({ kind: "workspace" });
});

it("leaves settings for the workspace before a shortcut opens an overlay there", () => {
  useWorkspace.getState().openSettings("general");
  press("n")?.run();
  expect([useWorkspace.getState().screen, useWorkspace.getState().overlay]).toEqual([{ kind: "workspace" }, { kind: "connection", editing: null }]);
});

it("closes settings, not the active tab, on the close-tab shortcut", () => {
  useWorkspace.getState().addTab({ id: "t1", title: "a.sql", console: { db: null, mode: "AutoCommit" }, table: null, file: null });
  useWorkspace.getState().openSettings();
  press("w")?.run();
  expect([useWorkspace.getState().screen, allCells(useWorkspace.getState())]).toEqual([{ kind: "workspace" }, ["t1"]]);
});

it.each(["w", ",", "t"])("drops the settings search when ⌘%s leaves settings, and keeps the tree search", (key) => {
  useWorkspace.getState().setTreeQuery("order");
  useWorkspace.getState().openSettings("general");
  useWorkspace.getState().setSettingsQuery("row");
  press(key)?.run();
  press(",")?.run();
  expect([useWorkspace.getState().screen, useWorkspace.getState().treeQuery]).toEqual([{ kind: "settings", section: "general", query: "" }, "order"]);
});

it("closes an open overlay when ⌘, opens settings", () => {
  useWorkspace.getState().open({ kind: "history" });
  press(",")?.run();
  expect([useWorkspace.getState().screen, useWorkspace.getState().overlay]).toEqual([{ kind: "settings", section: "general", query: "" }, { kind: "none" }]);
});

it("opens a section with a blank search when its nav entry is picked from the results", () => {
  useWorkspace.getState().openSettings("appearance");
  useWorkspace.getState().setSettingsQuery("row limit");
  useWorkspace.getState().openSettings("general");
  expect(useWorkspace.getState().screen).toEqual({ kind: "settings", section: "general", query: "" });
});

it("keeps a settings search out of the workspace screen", () => {
  useWorkspace.getState().setSettingsQuery("row");
  expect([useWorkspace.getState().screen, useWorkspace.getState().treeQuery]).toEqual([{ kind: "workspace" }, ""]);
});

it("shares the command registry with Cmd and Ctrl Shift+P while keeping plain P unchanged", () => {
  expect(press("P", { metaKey: true, shiftKey: true })).toBe(COMMANDS.find((c) => c.id === "commands"));
  expect(pressOff("P", { ctrlKey: true, shiftKey: true })?.id).toBe("commands");
  expect(press("p")?.id).toBe("navigation");
  expect(pressOff("p", { ctrlKey: true })?.id).toBe("navigation");
  expect(new Set(COMMANDS.map((c) => c.id)).size).toBe(COMMANDS.length);
  expect(COMMANDS.filter((c) => c.palette !== false)).toHaveLength(12);
  expect(START_COMMANDS.map((c) => c.id)).toEqual(["new-query", "open-file", "new-connection", "navigation", "saved-queries", "settings"]);
  expect(START_COMMANDS.every((c) => COMMANDS.includes(c))).toBe(true);
});

it("preserves a pending approval and every unrelated overlay on the command-panel shortcut", async () => {
  const answer = ask({ title: "Approve writes?", body: "", choices: [{ id: "run", label: "Run", variant: "danger" }] });
  const approval = useWorkspace.getState().overlay;
  press("p", { metaKey: true, shiftKey: true })?.run();
  expect(useWorkspace.getState().overlay).toBe(approval);
  useWorkspace.getState().answer("run");
  expect(await answer).toBe("run");
  for (const overlay of [{ kind: "history" }, { kind: "connection", editing: null }] as const) {
    useWorkspace.getState().open(overlay);
    openCommandPalette();
    expect(useWorkspace.getState().overlay).toBe(overlay);
  }
});

it("starts the notebook of a database that has none from ⌘T, labelled New notebook", () => {
  vi.stubGlobal("window", { crypto });
  const postgres = { connectionId: "c", database: "postgres" };
  const newQuery = START_COMMANDS.find((c) => c.id === "new-query")!;
  expect(commandLabel(newQuery)).toBe("New query tab");
  useWorkspace.getState().showDatabase(postgres);
  expect(commandLabel(newQuery)).toBe("New notebook");
  press("t")?.run();
  const notebook = shownNotebook(useWorkspace.getState());
  expect([notebook?.db, notebook?.title, notebook?.cells.length]).toEqual([postgres, "postgres", 1]);
  expect(commandLabel(newQuery)).toBe("New query tab");
});
