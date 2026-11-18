// @vitest-environment jsdom
import { undo } from "@codemirror/commands";
import { EditorSelection, EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, expect, it, vi } from "vitest";
import { commands, type AskOutcome, type ChatState, type Entry, type TurnOutcome } from "../../ipc/bindings";
import { useLive } from "../../state/live";
import { useWorkspace, type Tab } from "../../state/workspace";
import { cellsState, moveCell } from "../../testing/notebooks";
import {
  activateEditor,
  cursorOf,
  docOf,
  dropEditor,
  installEditorBehavior,
  insertAtCursor,
  mountEditor,
  moveCursor,
  seedEditor,
  textOf,
} from "./editorStates";
import { applyAsk, captureSql, discardAsk, dismissAsk, openAskAi, setAskPrompt, startAskAiSync, submitAsk, useAskAi } from "./askAi";

const TAB: Tab = {
  id: "ask-cell",
  title: "private-title",
  console: { db: { connectionId: "c1", database: "shop" }, schema: "public", mode: "AutoCommit" },
  table: null,
};
const options: ChatState["options"] = { provider: "Claude", model: "opus", effort: "high", permission: "Auto" };
const chatState = (id: string): ChatState => ({ id, rev: 1, title: "Ask", updatedAtMs: 1, entryCount: 0, status: { kind: "Draft" }, options });
let stopSync = () => {};
let unmount = () => {};
let view: EditorView | undefined;
let rev = 10;

function setup(sql = "SELECT 1;", selection?: { from: number; to: number }) {
  mockIPC(() => null);
  useWorkspace.setState(cellsState([TAB]));
  seedEditor(TAB.id, sql);
  activateEditor(TAB.id);
  const host = document.createElement("div");
  document.body.append(host);
  unmount = mountEditor(host);
  const editor = host.querySelector<HTMLElement>(".cm-editor");
  view = editor ? (EditorView.findFromDOM(editor) ?? undefined) : undefined;
  if (!view) throw new Error("Editor did not mount");
  if (selection) view.dispatch({ selection: { anchor: selection.from, head: selection.to } });
  stopSync = startAskAiSync();
  return view;
}

function terminal(chatId: string, turn: number, ask: AskOutcome | null, outcome: TurnOutcome = { kind: "Completed" }) {
  useLive
    .getState()
    .apply({ kind: "ChatEntryChanged", chatId, seq: turn + 1, turn, rev: ++rev, entry: { kind: "End", atMs: 1, outcome, suggestions: [], ask } });
}

function boundary() {
  const sent = vi.spyOn(commands, "assistantAsk").mockImplementation(async () => ({ status: "ok", data: { turn: 5 } }));
  const stopped = vi.spyOn(commands, "assistantStopTurn").mockResolvedValue();
  const opened = vi.spyOn(commands, "assistantOpen").mockImplementation(async (id) => ({ status: "ok", data: { state: chatState(id), entries: [] } }));
  return { sent, stopped, opened };
}

async function send(prompt = "Edit this") {
  openAskAi();
  setAskPrompt(prompt);
  await submitAsk();
  const state = useAskAi.getState().state;
  if (state.kind === "closed" || !state.session.chatId) throw new Error("No submitted chat");
  return state.session.chatId;
}

async function propose(sql: string) {
  const id = await send();
  terminal(id, 5, { kind: "Replacement", sql });
  expect(useAskAi.getState().state.kind).toBe("review");
  return id;
}

afterEach(() => {
  stopSync();
  unmount();
  dropEditor(TAB.id);
  dropEditor("other");
  document.body.replaceChildren();
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
  vi.restoreAllMocks();
  clearMocks();
  installEditorBehavior([]);
});

it("captures exact selection, whole cell, and whitespace-only generation without sharing surrounding SQL", () => {
  const sql = "private prefix\n  selected 🦀; \nprivate suffix";
  const from = sql.indexOf("  selected");
  const to = sql.indexOf("\nprivate suffix");
  const selected = captureSql(EditorState.create({ doc: sql, selection: EditorSelection.single(from, to) }));
  expect(selected.source).toEqual({ scope: "Selection", sql: "  selected 🦀; " });
  expect(selected.range).toEqual({ from, to });
  expect(captureSql(EditorState.create({ doc: sql })).source).toEqual({ scope: "Cell", sql });
  expect(captureSql(EditorState.create({ doc: " \n\t" })).source.sql).toBe(" \n\t");
  expect(captureSql(EditorState.create()).source.sql).toBe("");
});

it("opening and moving the cursor neither creates a chat nor recaptures its selection", () => {
  setup("SELECT 1;", { from: 7, to: 8 });
  const { opened, sent } = boundary();
  openAskAi();
  moveCursor(TAB.id, 0);
  const state = useAskAi.getState().state;
  expect(state.kind).toBe("prompt");
  if (state.kind !== "closed") expect(state.session.capture.source.sql).toBe("1");
  expect(opened).not.toHaveBeenCalled();
  expect(sent).not.toHaveBeenCalled();
});

it("sends only explicit selection and metadata while schema-only access stays unchanged", async () => {
  setup("SECRET PREFIX\n  SELECT 1; \nSECRET SUFFIX", { from: 14, to: 26 });
  const { sent } = boundary();
  await send();
  expect(sent.mock.calls[0]?.[1]).toEqual({
    text: "Edit this",
    tabId: TAB.id,
    db: TAB.console.db,
    schema: "public",
    source: { scope: "Selection", sql: "  SELECT 1; ", draft: null },
  });
  expect(useWorkspace.getState().settings.assistantAccess).toBe("SchemaOnly");
});

it("handles a terminal reply before the send acknowledgement and applies only the captured range in one Undo", async () => {
  const editor = setup("-- keep\nSELECT 1;\n-- also keep", { from: 15, to: 16 });
  const { sent } = boundary();
  sent.mockImplementation(async (id) => {
    terminal(id, 5, { kind: "Replacement", sql: "42" });
    return { status: "ok", data: { turn: 5 } };
  });
  const original = docOf(TAB.id);
  await send();
  expect(useAskAi.getState().state.kind).toBe("review");
  expect(docOf(TAB.id)).toBe(original);
  expect(applyAsk()).toBe(true);
  expect(textOf(TAB.id)).toBe("-- keep\nSELECT 42;\n-- also keep");
  expect(undo(editor)).toBe(true);
  expect(textOf(TAB.id)).toBe("-- keep\nSELECT 1;\n-- also keep");
  expect(undo(editor)).toBe(false);
});

it("accepts an empty replacement as deletion", async () => {
  setup("SELECT 1;");
  boundary();
  await propose("");
  expect(applyAsk()).toBe(true);
  expect(textOf(TAB.id)).toBe("");
});

it("invalidates proposals on editing, even if Undo restores the same characters", async () => {
  const editor = setup();
  boundary();
  await propose("SELECT 2;");
  insertAtCursor(TAB.id, " ");
  undo(editor);
  expect(textOf(TAB.id)).toBe("SELECT 1;");
  expect(useAskAi.getState().state.kind).toBe("closed");
  expect(applyAsk()).toBe(false);
});

it.each(["database", "schema", "tab", "closed", "settings"])("invalidates capture when %s changes", async (change) => {
  setup();
  boundary();
  await propose("SELECT 2;");
  if (change === "database") moveCell(TAB.id, { connectionId: "c1", database: "other" });
  if (change === "schema") useWorkspace.getState().patchConsole(TAB.id, { schema: "other" });
  if (change === "tab") {
    const other = { ...TAB, id: "other" };
    seedEditor("other", "SELECT 3;");
    useWorkspace.setState(cellsState([TAB, other], "other"));
  }
  if (change === "closed") {
    dropEditor(TAB.id);
    useWorkspace.getState().closeTab(TAB.id);
  }
  if (change === "settings") useWorkspace.getState().setSettings({ ...useWorkspace.getState().settings, assistantAccess: "Full" });
  expect(useAskAi.getState().state.kind).toBe("closed");
  expect(applyAsk()).toBe(false);
});

it("cancels only the late accepted turn after closing before acknowledgement", async () => {
  setup();
  const { sent, stopped } = boundary();
  let accept = (_result: Awaited<ReturnType<typeof commands.assistantAsk>>) => {};
  const acceptance = new Promise<Awaited<ReturnType<typeof commands.assistantAsk>>>((resolve) => {
    accept = resolve;
  });
  sent.mockReturnValue(acceptance);
  openAskAi();
  setAskPrompt("Edit");
  const sending = submitAsk();
  await vi.waitFor(() => expect(sent).toHaveBeenCalled());
  const id = sent.mock.calls[0]?.[0];
  dismissAsk();
  accept({ status: "ok", data: { turn: 91 } });
  await sending;
  expect(stopped).toHaveBeenCalledExactlyOnceWith(id, 91);
  expect(useAskAi.getState().state.kind).toBe("closed");
  if (id) terminal(id, 91, { kind: "Replacement", sql: "SELECT 9" });
  expect(textOf(TAB.id)).toBe("SELECT 1;");
});

it("retains the accepted draft through refinement cancellation, failure and question handoff", async () => {
  setup();
  const { sent, stopped } = boundary();
  const id = await propose("SELECT 2;");
  sent.mockResolvedValue({ status: "ok", data: { turn: 8 } });
  await send("Refine");
  expect(sent.mock.calls.at(-1)?.[1].source.draft).toBe("SELECT 2;");
  dismissAsk();
  expect(stopped).toHaveBeenCalledWith(id, 8);
  expect(useAskAi.getState().state.kind).toBe("review");
  sent.mockResolvedValue({ status: "ok", data: { turn: 10 } });
  await send("Try again");
  terminal(id, 10, null, { kind: "Failed", error: { kind: "Provider", message: "Unavailable" } });
  const failed = useAskAi.getState().state;
  expect(failed.kind).toBe("error");
  if (failed.kind !== "closed") expect(failed.session.draft).toBe("SELECT 2;");
  sent.mockResolvedValue({ status: "ok", data: { turn: 20 } });
  await send("Explain the draft");
  terminal(id, 20, { kind: "Answer" });
  expect(useWorkspace.getState().layout).toMatchObject({ rightPanel: "Assistant", assistantChat: id });
  expect(useAskAi.getState().state.kind).toBe("review");
  expect(textOf(TAB.id)).toBe("SELECT 1;");
  discardAsk();
});

it("clears an older draft when a successful refinement returns the original text", async () => {
  setup();
  const { sent } = boundary();
  const id = await propose("SELECT 2;");
  sent.mockResolvedValue({ status: "ok", data: { turn: 10 } });
  await send("Undo that proposal");
  terminal(id, 10, { kind: "Replacement", sql: "SELECT 1;" });
  const state = useAskAi.getState().state;
  expect(state.kind).toBe("prompt");
  if (state.kind === "prompt") expect([state.session.draft, state.message]).toEqual([null, "No changes."]);
  expect(applyAsk()).toBe(false);
});

it("ignores streamed SQL examples until the engine returns a terminal decision", async () => {
  setup();
  boundary();
  const id = await send();
  const entry: Entry = { kind: "Text", done: true, blocks: [{ kind: "Sql", sql: "SELECT 99", complete: true, vetted: true, relations: [] }] };
  useLive.getState().apply({ kind: "ChatEntryChanged", chatId: id, turn: 5, seq: 7, rev: ++rev, entry });
  expect(useAskAi.getState().state.kind).toBe("sending");
  terminal(id, 5, { kind: "Invalid", message: "Incomplete proposal" });
  expect(useAskAi.getState().state.kind).toBe("error");
  expect(applyAsk()).toBe(false);
  expect(textOf(TAB.id)).toBe("SELECT 1;");
});

it("invalidates during the editor cursor notification without dispatching a nested editor update", async () => {
  installEditorBehavior(
    EditorView.updateListener.of((update) => {
      if (update.docChanged) useWorkspace.getState().setCursor(cursorOf(update.state));
    }),
  );
  setup();
  boundary();
  await propose("SELECT 2;");
  const errors = vi.spyOn(console, "error");
  insertAtCursor(TAB.id, " ");
  expect(useAskAi.getState().state.kind).toBe("closed");
  expect(errors).not.toHaveBeenCalled();
  expect(textOf(TAB.id)).toBe(" SELECT 1;");
});
