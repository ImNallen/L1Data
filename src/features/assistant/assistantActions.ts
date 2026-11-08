import type { EditorState } from "@codemirror/state";
import {
  commands,
  type ApprovalId,
  type ChatId,
  type ChatOptions,
  type ChatState,
  type DatabaseRef,
  type Decision,
  type EffortId,
  type ModelOption,
  type ResultRef,
  type RunState,
  type TabContext,
  type TabId,
} from "../../ipc/bindings";
import { approveErrorText, chatErrorText, sendErrorText } from "../../ipc/errors";
import { useLive } from "../../state/live";
import { resultView, type ResultPick, type RowResult } from "../../state/status";
import { needsResync } from "../../state/transcript";
import { activeId, activeTab, confirm, useWorkspace, type Tab } from "../../state/workspace";
import { editorStateOf, focusEditor, insertAtCursor, replaceDoc } from "../editor/editorStates";
import { flushSettings, openSqlTab, runActive } from "../editor/runActions";

const ws = () => useWorkspace.getState();
const notify = (text: string) => ws().notify(text);

const opening = new Map<ChatId, Promise<void>>();

export function openChat(id: ChatId): Promise<void> {
  const inFlight = opening.get(id);
  if (inFlight) return inFlight;
  useLive.getState().seedTranscript(id);
  const open = commands
    .assistantOpen(id)
    .then((res) => {
      if (res.status === "error") return notify(chatErrorText(res.error));
      useLive.getState().hydrateChat(res.data);
    })
    .finally(() => opening.delete(id));
  opening.set(id, open);
  return open;
}

export function showChat(id: ChatId): Promise<void> {
  ws().setLayout({ assistantChat: id });
  return openChat(id);
}

export function newChat(): Promise<void> {
  return showChat(crypto.randomUUID());
}

export function gridResult(run: RunState | undefined, picked: ResultPick | undefined, hasOutput: boolean): RowResult | null {
  if (!run) return null;
  const view = resultView(run, picked, hasOutput, "default");
  return view.kind === "rows" ? view.result : null;
}

function shownResultRef(tabId: TabId): ResultRef | null {
  const live = useLive.getState();
  return gridResult(live.consoles[tabId]?.run, ws().activeResult[tabId], (live.output[tabId]?.entries.length ?? 0) > 0)?.meta.ref ?? null;
}

export function tabContext(tab: Tab, editor: EditorState, attach: ResultRef | null): TabContext {
  const sel = editor.selection.main;
  return {
    tabId: tab.id,
    title: tab.title,
    db: tab.console.db,
    text: editor.doc.toString(),
    target: sel.empty ? { kind: "cursor", offset: sel.head } : { kind: "selection", from: sel.from, to: sel.to },
    attach,
  };
}

export async function sendMessage(chatId: ChatId, text: string, attach: boolean): Promise<boolean> {
  try {
    await flushSettings();
  } catch {
    notify("Could not save settings. Save assistant access again before sending.");
    return false;
  }
  const { settings } = ws();
  const tab = activeTab(ws());
  const full = settings.assistantAccess === "Full";
  const context = tab ? tabContext(tab, editorStateOf(tab.id), full && attach ? shownResultRef(tab.id) : null) : null;
  if (context && !full) {
    context.text = "";
    context.title = "";
    context.target = { kind: "all" };
  }
  const res = await commands.assistantSend(chatId, { text, tab: context });
  if (res.status === "error") {
    notify(sendErrorText(res.error));
    return false;
  }
  return true;
}

export function stopTurn(chatId: ChatId): void {
  void commands.assistantStop(chatId);
}

export async function answerApproval(chatId: ChatId, approval: ApprovalId, decision: Decision): Promise<void> {
  const res = await commands.assistantApprove(chatId, approval, decision);
  if (res.status === "error") notify(approveErrorText(res.error));
}

export function effortFor(model: ModelOption, current: EffortId | null): EffortId | null {
  const ids = model.efforts.map((e) => e.id);
  if (current !== null && ids.includes(current)) return current;
  if (ids.includes("high")) return "high";
  return ids.at(-1) ?? null;
}

export async function configureChat(chatId: ChatId, options: ChatOptions): Promise<void> {
  const res = await commands.assistantConfigure(chatId, options);
  if (res.status === "error") return notify(chatErrorText(res.error));
  useLive.getState().hydrateChat({ state: res.data, entries: [] });
}

export async function confirmDeleteChat(chat: ChatState): Promise<void> {
  const ok = await confirm({
    title: "Delete this chat?",
    body: `This removes the chat from L1 Data. ${chat.options.provider} keeps its own copy of the conversation in its config directory.`,
    confirmLabel: "Delete",
  });
  if (ok) await commands.assistantDelete(chat.id);
}

export function runInNewTab(sql: string, db: DatabaseRef | null): void {
  openSqlTab({ db: db ?? undefined, text: sql });
  runActive({ kind: "all" });
}

export function openInNewTab(sql: string, db: DatabaseRef | null): void {
  openSqlTab({ db: db ?? undefined, text: sql });
}

export function insertSql(sql: string): void {
  const id = activeId(ws());
  if (!id) return void openSqlTab({ text: sql });
  insertAtCursor(id, sql);
  focusEditor();
}

export function replaceTabSql(sql: string): void {
  const id = activeId(ws());
  if (!id) return void openSqlTab({ text: sql });
  replaceDoc(id, sql);
  focusEditor();
}

export function startAssistantSync(): () => void {
  const reopenedGap = new Map<ChatId, string>();
  return useLive.subscribe((s, prev) => {
    if (s.chats === prev.chats) return;
    for (const chat of Object.values(s.chats)) {
      const held = s.transcripts[chat.id];
      if (chat === prev.chats[chat.id] || !held || !needsResync(chat, held)) continue;
      const gap = `${chat.entryCount}:${Object.keys(held).length}`;
      if (reopenedGap.get(chat.id) === gap) continue;
      reopenedGap.set(chat.id, gap);
      void openChat(chat.id);
    }
  });
}
