import type { HistoryGroupSummary } from "../../ipc/bindings";
import { focusEditor } from "../editor/editorStates";
import { insertIntoActive, openSqlTab, runActive } from "../editor/runActions";

export function openHistoryQuery(group: HistoryGroupSummary, sql: string, run = false) {
  openSqlTab({ db: { connectionId: group.connectionId, database: group.database }, text: sql, title: group.title });
  if (run) runActive({ kind: "all" });
}

export function insertHistoryQuery(sql: string): boolean {
  if (!insertIntoActive(sql)) return false;
  focusEditor();
  return true;
}
