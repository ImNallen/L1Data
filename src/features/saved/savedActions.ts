import {
  commands,
  type DatabaseKind,
  type DatabaseRef,
  type SavedQuery,
  type SavedQueryEdit,
  type SavedQueryError,
  type SavedQueryId,
  type SaveQueryReply,
} from "../../ipc/bindings";
import { savedQueryErrorText } from "../../ipc/errors";
import { databaseKindFor } from "../../state/databaseKind";
import { homeDb } from "../../state/dbKey";
import { useLive } from "../../state/live";
import { activeId, activeTab, tabTarget, useWorkspace } from "../../state/workspace";
import { editorStateOf, insertAtCursor } from "../editor/editorStates";
import { isBrowsing, openSqlTab, selectionTarget, textTarget } from "../editor/runActions";

const ws = () => useWorkspace.getState();

function showSaved(reply: SaveQueryReply) {
  useLive.getState().hydrateSaved(reply.queries);
  ws().open({ kind: "saved", entry: reply.alreadySaved ? { kind: "existing", id: reply.id } : { kind: "name", id: reply.id } });
}

export async function saveActiveQuery(): Promise<void> {
  const s = ws();
  if (s.screen.kind !== "workspace" || s.overlay.kind !== "none") return;
  const tab = activeTab(s);
  if (!tab || isBrowsing(tab)) return s.notify("Open a query tab to save a query.");
  const state = editorStateOf(tab.id);
  const res = await commands.savedQuerySave(state.doc.toString(), selectionTarget(state), textTarget(tab).databaseKind);
  if (res.status === "error") return ws().notify(savedQueryErrorText(res.error));
  showSaved(res.data);
}

export async function saveHistoryQuery(sql: string, databaseKind: DatabaseKind): Promise<void> {
  ws().close();
  const res = await commands.savedQuerySave(sql, { kind: "all" }, databaseKind);
  if (res.status === "error") return ws().notify(savedQueryErrorText(res.error));
  showSaved(res.data);
}

export function canInsert(): boolean {
  const s = ws();
  const tab = activeTab(s);
  return tab !== undefined && !isBrowsing(tab);
}

function markUsed(id: SavedQueryId): void {
  void commands.savedQueryUsed(id).then((res) => {
    if (res.status === "ok") useLive.getState().hydrateSaved(res.data);
  });
}

export function insertSaved(q: SavedQuery): void {
  const id = activeId(ws());
  if (!id) return;
  insertAtCursor(id, q.sql);
  markUsed(q.id);
}

function targetFor(kind: DatabaseKind): DatabaseRef | null {
  const s = ws();
  const active = tabTarget(activeTab(s));
  if (active && databaseKindFor(active, s.configs) === kind) return active;
  const live = useLive.getState().connections;
  const ofKind = Object.values(s.configs).filter((c) => c.databaseKind === kind);
  const config = ofKind.find((c) => live[c.id]?.status.kind === "Connected") ?? ofKind[0];
  return config ? homeDb(config, live[config.id]) : null;
}

export function openSaved(q: SavedQuery): void {
  openSqlTab({ db: targetFor(q.databaseKind), text: q.sql, title: q.name });
  markUsed(q.id);
}

/** The clipboard write starts before anything awaits, so WebKit still counts it as part of the user's key press or click. */
export async function copySaved(q: SavedQuery): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(q.sql);
  } catch {
    return false;
  }
  markUsed(q.id);
  ws().notify(`Copied “${q.name}”.`);
  return true;
}

export async function updateSaved(id: SavedQueryId, edit: SavedQueryEdit): Promise<SavedQueryError | null> {
  const res = await commands.savedQueryUpdate(id, edit);
  if (res.status === "error") return res.error;
  useLive.getState().hydrateSaved(res.data);
  return null;
}

export async function deleteSaved(id: SavedQueryId): Promise<void> {
  const res = await commands.savedQueryDelete(id);
  if (res.status === "error") ws().notify(savedQueryErrorText(res.error));
  else useLive.getState().hydrateSaved(res.data);
}
