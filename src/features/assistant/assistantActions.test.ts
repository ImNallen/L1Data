import { undo } from "@codemirror/commands";
import { EditorSelection, EditorState } from "@codemirror/state";
import { mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, expect, it, vi } from "vitest";
import type { ChatState, ChatStatus, ConsoleState, EntrySnapshot, ModelOption, ResultMeta, SendRequest } from "../../ipc/bindings";
import { useLive } from "../../state/live";
import { useWorkspace, type Tab } from "../../state/workspace";
import { dropEditor, editorStateOf, insertAtCursor, seedEditor, textOf } from "../editor/editorStates";
import { confirmDeleteChat, effortFor, openChat, replaceTabSql, sendMessage, startAssistantSync, tabContext } from "./assistantActions";

afterEach(() => {
  vi.unstubAllGlobals();
  for (const id of useWorkspace.getState().order) dropEditor(id);
  useWorkspace.setState(useWorkspace.getInitialState(), true);
  useLive.setState(useLive.getInitialState(), true);
});

const TAB: Tab = {
  id: "tab-1",
  title: "orders_by_region.sql",
  console: { db: { connectionId: "c1", database: "shop" }, mode: "AutoCommit" },
  table: null,
  file: null,
};

const DOC = "-- 🦀 crab\nselect 1;\nselect 2;";

it("sends a selection as UTF-16 offsets into the tab's text, with the tab's database and title", () => {
  const editor = EditorState.create({ doc: DOC, selection: EditorSelection.single(21, 30) });
  expect(tabContext(TAB, editor, null)).toEqual({
    tabId: "tab-1",
    title: "orders_by_region.sql",
    db: { connectionId: "c1", database: "shop" },
    text: DOC,
    target: { kind: "selection", from: 21, to: 30 },
    attach: null,
  });
  expect(DOC.slice(21, 30)).toBe("select 2;");
});

it("sends a bare cursor as one UTF-16 offset", () => {
  const editor = EditorState.create({ doc: DOC, selection: EditorSelection.cursor(18) });
  expect(tabContext(TAB, editor, { tabId: "tab-1", runId: 4, index: 0 }).target).toEqual({ kind: "cursor", offset: 18 });
  expect(DOC.slice(11, 18)).toBe("select ");
});

const rows = (runId: number): ResultMeta => ({
  ref: { tabId: "tab-1", runId, index: 0 },
  db: { connectionId: "c1", database: "shop" },
  databaseKind: "Postgres",
  columns: [],
  rowCount: 6,
  rowsSeen: 6,
  truncated: null,
  elapsedMs: 27,
  allRows: { kind: "Statement" },
});

function showTab(text: string, live?: ConsoleState) {
  useWorkspace.setState((s) => ({ settings: { ...s.settings, assistantAccess: "Full" } }));
  seedEditor(TAB.id, "");
  insertAtCursor(TAB.id, text);
  useWorkspace.setState({ tabs: { [TAB.id]: TAB }, order: [TAB.id], active: TAB.id });
  if (live) useLive.setState({ consoles: { [TAB.id]: live } });
}

function captureSend() {
  vi.stubGlobal("window", {});
  const sent: { chatId: string; request: SendRequest }[] = [];
  mockIPC((cmd, args) => {
    if (cmd !== "assistant_send") return;
    sent.push(args as { chatId: string; request: SendRequest });
    return { turn: 0 };
  });
  return sent;
}

it("sends the active tab's text, cursor, and the result its grid shows when attach is on", async () => {
  const sent = captureSend();
  const run: ConsoleState["run"] = {
    kind: "Finished",
    runId: 4,
    elapsedMs: 30,
    outcome: "Ok",
    statements: [{ index: 0, range: { from: 0, to: 9 }, status: { kind: "Finished" }, outputs: [{ kind: "Rows", result: rows(4), state: "Complete" }] }],
  };
  showTab(DOC, {
    tabId: TAB.id,
    rev: 3,
    config: { connectionId: "c1", database: "shop", schema: null, mode: "AutoCommit" },
    session: { kind: "None" },
    txn: "Idle",
    txnDepth: null,
    work: null,
    export: null,
    import: null,
    run,
  });

  expect(await sendMessage("chat-1", "Which regions grew?", true)).toBe(true);
  expect(await sendMessage("chat-1", "And without it?", false)).toBe(true);
  expect(sent).toEqual([
    {
      chatId: "chat-1",
      request: {
        text: "Which regions grew?",
        tab: {
          tabId: "tab-1",
          title: "orders_by_region.sql",
          db: { connectionId: "c1", database: "shop" },
          text: DOC,
          target: { kind: "cursor", offset: 30 },
          attach: { tabId: "tab-1", runId: 4, index: 0 },
        },
      },
    },
    {
      chatId: "chat-1",
      request: {
        text: "And without it?",
        tab: {
          tabId: "tab-1",
          title: "orders_by_region.sql",
          db: { connectionId: "c1", database: "shop" },
          text: DOC,
          target: { kind: "cursor", offset: 30 },
          attach: null,
        },
      },
    },
  ]);
});

it("sends no tab when none is open, and toasts the engine's refusal", async () => {
  vi.stubGlobal("window", {});
  const sent: unknown[] = [];
  mockIPC((cmd, args) => {
    if (cmd !== "assistant_send") return;
    sent.push(args);
    throw { kind: "Busy" };
  });
  expect(await sendMessage("chat-1", "hello", true)).toBe(false);
  expect(sent).toEqual([{ chatId: "chat-1", request: { text: "hello", tab: null } }]);
  expect(useWorkspace.getState().toast?.text).toBe("The assistant is still answering. Stop it or wait for it to finish.");
});

it("replaces the active tab's SQL as one edit that a single undo reverts", () => {
  showTab("select 1;\n");
  insertAtCursor(TAB.id, "select 2;");
  replaceTabSql("SELECT region FROM customers;");
  expect(textOf(TAB.id)).toBe("SELECT region FROM customers;");
  let after = editorStateOf(TAB.id);
  undo({ state: after, dispatch: (tr) => void (after = tr.state) });
  expect(after.doc.toString()).toBe("select 1;\nselect 2;");
});

const CHAT = "0b7d5a52-9f1e-4c55-9d43-3c1f3c6e2a10";
const IDLE: ChatStatus = { kind: "Idle" };
const chatAt = (rev: number, status: ChatStatus, entryCount: number): ChatState => ({
  id: CHAT,
  rev,
  title: "Which regions grew",
  updatedAtMs: 1,
  options: { provider: "Claude", model: "opus", effort: "high", permission: "AskFirst" },
  status,
  entryCount,
});
const notice = (seq: number, rev: number, text: string): EntrySnapshot => ({ chatId: CHAT, seq, rev, turn: 0, entry: { kind: "Notice", text } });
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

it("reopens a chat once per gap, even when the reopen leaves the gap open and a newer rev follows", async () => {
  vi.stubGlobal("window", {});
  const opened: unknown[] = [];
  mockIPC((cmd, args) => {
    if (cmd !== "assistant_open") return;
    opened.push(args);
    return { state: chatAt(100 + opened.length, IDLE, 3), entries: [notice(0, 1, "first")] };
  });
  const stop = startAssistantSync();
  try {
    useLive.setState({
      chats: { [CHAT]: chatAt(5, { kind: "Running", turn: 0, startedAtMs: 1, phase: "Working" }, 1) },
      transcripts: { [CHAT]: { 0: notice(0, 1, "first") } },
    });
    useLive.getState().apply({ kind: "ChatChanged", ...chatAt(6, IDLE, 3) });
    await settle();
    useLive.getState().apply({ kind: "ChatChanged", ...chatAt(102, IDLE, 3) });
    await settle();
    expect(opened).toEqual([{ chatId: CHAT }]);

    useLive.getState().apply({ kind: "ChatChanged", ...chatAt(200, IDLE, 4) });
    await settle();
    expect(opened).toEqual([{ chatId: CHAT }, { chatId: CHAT }]);
  } finally {
    stop();
  }
});

it("keeps an entry event that arrives while the chat opens over the older copy in the reply", async () => {
  vi.stubGlobal("window", {});
  mockIPC((cmd) => {
    if (cmd !== "assistant_open") return;
    useLive.getState().apply({ kind: "ChatEntryChanged", ...notice(1, 9, "newer") });
    return { state: chatAt(8, IDLE, 2), entries: [notice(0, 1, "first"), notice(1, 7, "older")] };
  });
  await openChat(CHAT);
  expect(useLive.getState().transcripts).toEqual({ [CHAT]: { 0: notice(0, 1, "first"), 1: notice(1, 9, "newer") } });
});

it("deletes a chat only after the user confirms, and says the provider keeps its own copy", async () => {
  vi.stubGlobal("window", {});
  const deleted: unknown[] = [];
  mockIPC((cmd, args) => {
    if (cmd === "assistant_delete") deleted.push(args);
  });
  void confirmDeleteChat(chatAt(4, IDLE, 2));
  const overlay = useWorkspace.getState().overlay;
  if (overlay.kind !== "ask") throw new Error(`expected a confirmation, got ${overlay.kind}`);
  expect([overlay.title, overlay.body, overlay.choices.map((c) => c.label).join(), deleted.length]).toEqual([
    "Delete this chat?",
    "This removes the chat from L1 Data. Claude keeps its own copy of the conversation in its config directory.",
    "Delete",
    0,
  ]);
  useWorkspace.getState().answer("btn");
  await settle();
  expect(deleted).toEqual([{ chatId: CHAT }]);
});

const model = (...efforts: string[]): ModelOption => ({
  id: "m",
  label: "M",
  description: "",
  recommended: false,
  efforts: efforts.map((id) => ({ id, label: id })),
});

it("keeps the effort across a model switch when the model lists it, else high, else the model's last level, else none", () => {
  expect([
    effortFor(model("low", "medium", "high", "max"), "max"),
    effortFor(model("low", "medium", "high"), "max"),
    effortFor(model("low", "medium", "high"), null),
    effortFor(model("low", "medium"), "max"),
    effortFor(model(), "high"),
  ]).toEqual(["max", "high", "high", "medium", null]);
});

it("drops a stale attachment request and editor text in schema only mode", async () => {
  const sent = captureSend();
  showTab("select 'EDITOR_CANARY'");
  useWorkspace.setState((s) => ({ settings: { ...s.settings, assistantAccess: "SchemaOnly" } }));
  expect(await sendMessage("chat-1", "My pasted text", true)).toBe(true);
  expect(sent).toEqual([
    {
      chatId: "chat-1",
      request: { text: "My pasted text", tab: { tabId: TAB.id, db: TAB.console.db, title: "", text: "", target: { kind: "all" }, attach: null } },
    },
  ]);
});

it("waits for every queued access change before admitting a message", async () => {
  const { saveSettings } = await import("../editor/runActions");
  vi.stubGlobal("window", {});
  const modes: unknown[] = [];
  const sent: unknown[] = [];
  let release = () => {};
  mockIPC((cmd, args) => {
    if (cmd === "settings_set") {
      modes.push(args);
      if (modes.length === 1)
        return new Promise<void>((resolve) => {
          release = resolve;
        });
    }
    if (cmd === "assistant_send") {
      sent.push(args);
      return { turn: 0 };
    }
  });
  const full = saveSettings({ assistantAccess: "Full" });
  const schema = saveSettings({ assistantAccess: "SchemaOnly" });
  const sending = sendMessage("chat-1", "Schema help", true);
  await vi.waitFor(() => expect(modes).toHaveLength(1));
  expect(sent).toHaveLength(0);
  release();
  await Promise.all([full, schema, sending]);
  expect(modes).toEqual([
    { settings: expect.objectContaining({ assistantAccess: "Full" }) },
    { settings: expect.objectContaining({ assistantAccess: "SchemaOnly" }) },
  ]);
  expect(sent).toEqual([{ chatId: "chat-1", request: { text: "Schema help", tab: null } }]);
});

it("uses the active tab at send time for the same chat, then sends no target when it is cleared", async () => {
  const sent = captureSend();
  showTab("select 1");
  expect(await sendMessage("same-chat", "First", false)).toBe(true);
  const second: Tab = { ...TAB, id: "tab-2", console: { ...TAB.console, db: { connectionId: "c2", database: "reporting" } } };
  seedEditor(second.id, "select 2");
  useWorkspace.getState().addTab(second);
  expect(await sendMessage("same-chat", "Second", false)).toBe(true);
  useWorkspace.setState({ active: null });
  expect(await sendMessage("same-chat", "No target", false)).toBe(true);
  expect(sent.map((s) => s.request.tab?.db ?? null)).toEqual([TAB.console.db, second.console.db, null]);
});
