import type { EditorState, Text } from "@codemirror/state";
import { create } from "zustand";
import { commands, type AskSource, type ChatId, type ChatOptions, type DatabaseKind, type DatabaseRef, type DocRange, type TabId } from "../../ipc/bindings";
import { chatErrorText, sendErrorText } from "../../ipc/errors";
import { databaseKindFor } from "../../state/databaseKind";
import { useLive } from "../../state/live";
import { activeId, activeQueryTab, useWorkspace } from "../../state/workspace";
import { agentErrorText } from "../assistant/assistantText";
import { showChat } from "../assistant/assistantActions";
import { applyCapturedEdit, docOf, editorStateOf, focusEditor, onBuffersChanged, showAskPreview } from "./editorStates";
import { flushSettings } from "./runActions";

export type AskCapture = {
  tabId: TabId;
  doc: Text;
  range: DocRange;
  source: Omit<AskSource, "draft">;
  db: DatabaseRef | null;
  schema: string | null;
  databaseKind: DatabaseKind;
  settingsKey: string;
  connectionKey: string;
  anchor: number;
};
type Session = {
  id: number;
  capture: AskCapture;
  chatId: ChatId | null;
  options: ChatOptions | null;
  chatOptionsKey: string | null;
  draft: string | null;
  prompt: string;
};
export type AskState =
  | { kind: "closed" }
  | { kind: "prompt"; session: Session; message: string | null }
  | { kind: "sending"; session: Session; request: number; accepted: { kind: "pending" } | { kind: "accepted"; turn: number } }
  | { kind: "review"; session: Session & { draft: string } }
  | { kind: "error"; session: Session; message: string };

export const useAskAi = create<{ state: AskState; previewHeight: number }>(() => ({ state: { kind: "closed" }, previewHeight: 0 }));
let sequence = 0;
const workspace = () => useWorkspace.getState();
const settingsKey = () => JSON.stringify([workspace().settings.assistantAccess, workspace().settings.providers]);
const connectionKey = (db: DatabaseRef | null) => JSON.stringify(db ? workspace().configs[db.connectionId] : null);

export function captureSql(editor: EditorState): Pick<AskCapture, "doc" | "range" | "source" | "anchor"> {
  const selection = editor.selection.main;
  const range = selection.empty ? { from: 0, to: editor.doc.length } : { from: selection.from, to: selection.to };
  return {
    doc: editor.doc,
    range,
    source: { scope: selection.empty ? "Cell" : "Selection", sql: editor.doc.sliceString(range.from, range.to) },
    anchor: selection.from,
  };
}

function valid(capture: AskCapture): boolean {
  const tab = activeQueryTab(workspace());
  return (
    tab?.id === capture.tabId &&
    !tab.collapsed &&
    docOf(capture.tabId) === capture.doc &&
    JSON.stringify(tab.console.db) === JSON.stringify(capture.db) &&
    (tab.console.schema ?? null) === capture.schema &&
    databaseKindFor(tab.console.db, workspace().configs) === capture.databaseKind &&
    settingsKey() === capture.settingsKey &&
    connectionKey(capture.db) === capture.connectionKey
  );
}

function setState(state: AskState): void {
  const before = useAskAi.getState().state;
  useAskAi.setState({ state, ...(state.kind === "closed" || state.session.draft === null ? { previewHeight: 0 } : {}) });
  if (before.kind !== "closed" && (state.kind === "closed" || before.session.capture !== state.session.capture || state.session.draft === null)) {
    const { tabId, doc } = before.session.capture;
    // The document transaction already cleared its preview, and may still be notifying workspace listeners.
    if (docOf(tabId) === doc) showAskPreview(tabId, null);
  }
  if (
    state.kind !== "closed" &&
    state.session.draft !== null &&
    (before.kind === "closed" || before.session.capture !== state.session.capture || before.session.draft !== state.session.draft)
  ) {
    const { capture, draft } = state.session;
    showAskPreview(capture.tabId, { doc: capture.doc, range: capture.range, sql: draft });
  }
}

function stop(state: AskState): void {
  if (state.kind === "sending" && state.accepted.kind === "accepted" && state.session.chatId) {
    void commands.assistantStopTurn(state.session.chatId, state.accepted.turn);
  }
}

export function discardAsk(): void {
  const before = useAskAi.getState().state;
  setState({ kind: "closed" });
  stop(before);
}

export function dismissAsk(): void {
  const before = useAskAi.getState().state;
  if (before.kind === "closed") return;
  const { session } = before;
  if (before.kind !== "review" && session.draft !== null) setState({ kind: "review", session: { ...session, draft: session.draft } });
  else setState({ kind: "closed" });
  stop(before);
  focusEditor();
}

export function openAskAi(): void {
  const held = useAskAi.getState().state;
  if (held.kind !== "closed" && valid(held.session.capture)) {
    if (held.kind !== "sending") setState({ kind: "prompt", session: { ...held.session, prompt: "" }, message: null });
    return;
  }
  discardAsk();
  const tab = activeQueryTab(workspace());
  if (!tab || tab.collapsed) return;
  const selectedChat = workspace().layout.assistantChat;
  const options = selectedChat ? (useLive.getState().chats[selectedChat]?.options ?? null) : null;
  const capture: AskCapture = {
    ...captureSql(editorStateOf(tab.id)),
    tabId: tab.id,
    db: tab.console.db,
    schema: tab.console.schema ?? null,
    databaseKind: databaseKindFor(tab.console.db, workspace().configs),
    settingsKey: settingsKey(),
    connectionKey: connectionKey(tab.console.db),
  };
  setState({ kind: "prompt", session: { id: ++sequence, capture, chatId: null, options, chatOptionsKey: null, draft: null, prompt: "" }, message: null });
}

export function setAskPrompt(prompt: string): void {
  const current = useAskAi.getState().state;
  if (current.kind === "prompt" || current.kind === "error") setState({ ...current, session: { ...current.session, prompt } });
}

function pending(id: number, request: number): Extract<AskState, { kind: "sending" }> | null {
  const state = useAskAi.getState().state;
  return state.kind === "sending" && state.session.id === id && state.request === request && valid(state.session.capture) ? state : null;
}

function fail(id: number, request: number, message: string): void {
  const current = pending(id, request);
  if (current) setState({ kind: "error", session: current.session, message });
}

function consumeReply(): void {
  const state = useAskAi.getState().state;
  if (state.kind !== "sending" || state.accepted.kind !== "accepted" || !state.session.chatId) return;
  const { turn } = state.accepted;
  const entries = useLive.getState().transcripts[state.session.chatId];
  const end = Object.values(entries ?? {}).find((entry) => entry.turn === turn && entry.entry.kind === "End")?.entry;
  if (!end || end.kind !== "End") return;
  if (!valid(state.session.capture)) {
    discardAsk();
    return;
  }
  const { session } = state;
  if (end.outcome.kind !== "Completed") {
    const message =
      end.outcome.kind === "Failed"
        ? agentErrorText(end.outcome.error, { name: session.options?.provider ?? "Assistant", signInCommand: "" }).text
        : "The request was stopped. Your SQL is unchanged.";
    setState({ kind: "error", session, message });
    return;
  }
  const outcome = end.ask;
  if (!outcome || outcome.kind === "Invalid") {
    setState({ kind: "error", session, message: outcome?.message ?? "The assistant did not return a complete answer. Try again." });
  } else if (outcome.kind === "Replacement") {
    if (outcome.sql === session.capture.source.sql) {
      setState({ kind: "prompt", session: { ...session, draft: null, prompt: "" }, message: "No changes." });
    } else setState({ kind: "review", session: { ...session, draft: outcome.sql, prompt: "" } });
  } else {
    if (session.draft !== null) setState({ kind: "review", session: { ...session, draft: session.draft, prompt: "" } });
    else setState({ kind: "closed" });
    workspace().setLayout({ rightPanel: "Assistant", assistantChat: state.session.chatId });
    void showChat(state.session.chatId);
  }
}

export async function submitAsk(): Promise<void> {
  const current = useAskAi.getState().state;
  if ((current.kind !== "prompt" && current.kind !== "error") || !current.session.prompt.trim()) return;
  if (!valid(current.session.capture)) {
    discardAsk();
    return;
  }
  const session = { ...current.session, chatId: current.session.chatId ?? crypto.randomUUID() };
  const request = ++sequence;
  setState({ kind: "sending", session, request, accepted: { kind: "pending" } });
  try {
    await flushSettings();
    if (!pending(session.id, request)) return;
    useLive.getState().seedTranscript(session.chatId);
    const opened = await commands.assistantOpen(session.chatId);
    if (!pending(session.id, request)) return;
    if (opened.status === "error") {
      fail(session.id, request, chatErrorText(opened.error));
      return;
    }
    useLive.getState().hydrateChat(opened.data);
    if (session.options && JSON.stringify(session.options) !== JSON.stringify(opened.data.state.options)) {
      const configured = await commands.assistantConfigure(session.chatId, session.options);
      if (!pending(session.id, request)) return;
      if (configured.status === "error") {
        fail(session.id, request, chatErrorText(configured.error));
        return;
      }
      useLive.getState().hydrateChat({ state: configured.data, entries: [] });
    }
    if (!pending(session.id, request)) return;
    const options = useLive.getState().chats[session.chatId]?.options ?? opened.data.state.options;
    setState({ kind: "sending", session: { ...session, options, chatOptionsKey: JSON.stringify(options) }, request, accepted: { kind: "pending" } });
    const { capture } = session;
    const accepted = await commands.assistantAsk(session.chatId, {
      text: session.prompt,
      tabId: capture.tabId,
      db: capture.db,
      schema: capture.schema,
      source: { ...capture.source, draft: session.draft },
    });
    const still = pending(session.id, request);
    if (!still) {
      if (accepted.status === "ok") void commands.assistantStopTurn(session.chatId, accepted.data.turn);
      return;
    }
    if (accepted.status === "error") {
      fail(session.id, request, sendErrorText(accepted.error));
      return;
    }
    setState({ ...still, accepted: { kind: "accepted", turn: accepted.data.turn } });
    consumeReply();
  } catch (error) {
    fail(session.id, request, error instanceof Error ? error.message : "Could not send the request. Try again.");
  }
}

export function applyAsk(): boolean {
  const state = useAskAi.getState().state;
  if (state.kind !== "review") return false;
  const { capture, draft } = state.session;
  setState({ kind: "closed" });
  if (!valid(capture)) return false;
  const applied = applyCapturedEdit(capture.tabId, capture.doc, { ...capture.range, insert: draft });
  focusEditor();
  return applied;
}

export function startAskAiSync(): () => void {
  const invalidate = () => {
    const state = useAskAi.getState().state;
    if (state.kind === "closed") return;
    const { session } = state;
    const optionsChanged =
      session.chatOptionsKey !== null &&
      session.chatId !== null &&
      JSON.stringify(useLive.getState().chats[session.chatId]?.options) !== session.chatOptionsKey;
    if (!valid(session.capture) || activeId(workspace()) !== session.capture.tabId || optionsChanged) discardAsk();
  };
  const buffers = onBuffersChanged(invalidate);
  const workspaces = useWorkspace.subscribe(invalidate);
  const live = useLive.subscribe(() => {
    invalidate();
    consumeReply();
  });
  return () => {
    buffers();
    workspaces();
    live();
    discardAsk();
  };
}
