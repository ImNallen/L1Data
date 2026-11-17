import type { Extension } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { commands, type CellSource, type TabId } from "../../ipc/bindings";
import { databaseKindFor } from "../../state/databaseKind";
import { notebookOf } from "../../state/notebooks";
import { activeId, activeQueryTab, isSqlTab, useWorkspace, type SqlTab } from "../../state/workspace";
import { provideCellNames } from "../editor/completion";
import { docText } from "../editor/docText";
import { docOf, editorStateOf, referenceAt, showReferences } from "../editor/editorStates";
import { goToCell } from "./cellNavigation";
import { staleness, stalenessText } from "./references";

const ws = () => useWorkspace.getState();

function sqlCells(id: TabId): SqlTab[] {
  const s = ws();
  return (notebookOf(s.notebooks, id)?.cells ?? []).flatMap((c) => {
    const tab = s.tabs[c];
    return isSqlTab(tab) ? [tab] : [];
  });
}

/** Every cell of the tab's notebook, by title, for the engine to expand `{{name}}` from. */
export function notebookSources(id: TabId): CellSource[] {
  return sqlCells(id).flatMap((t) => {
    const doc = docOf(t.id);
    return doc ? [{ name: t.title, text: docText(doc) }] : [];
  });
}

/** Why the cell's result no longer matches the cells it references, or "" while it does. */
export function staleReason(id: TabId): string {
  const sources = ws().runSources[id];
  return sources?.length ? stalenessText(staleness(sources, notebookSources(id))) : "";
}

let timer: ReturnType<typeof setTimeout> | undefined;

/** Underlines the active cell's references, and marks the ones no cell is named after. */
export function refreshReferences(): void {
  clearTimeout(timer);
  timer = setTimeout(() => void markReferences(), 60);
}

async function markReferences(): Promise<void> {
  const tab = activeQueryTab(ws());
  if (!tab) return;
  const id = tab.id;
  const doc = editorStateOf(id).doc;
  // A doc without "{{" holds no reference, so most edits skip the engine.
  if (!docText(doc).includes("{{")) return showReferences(id, doc, []);
  const found = await commands.cellReferences(docText(doc), databaseKindFor(tab.console.db, ws().configs));
  // A reply for an older doc or another cell is dropped: the change that made it stale already scheduled a newer pass.
  if (activeId(ws()) !== id || editorStateOf(id).doc !== doc) return;
  const names = new Set(sqlCells(id).map((t) => t.title));
  showReferences(
    id,
    doc,
    found.map((r) => ({ from: r.range.from, to: r.range.to, name: r.name, known: names.has(r.name) })),
  );
}

function jumpTo(from: TabId, name: string): boolean {
  const target = sqlCells(from).find((t) => t.title === name);
  if (!target || target.id === from) return false;
  goToCell(target.id);
  return true;
}

export function installCellReferences(): Extension {
  provideCellNames(() => {
    const id = activeId(ws());
    return id
      ? sqlCells(id)
          .filter((t) => t.id !== id)
          .map((t) => t.title)
      : [];
  });
  useWorkspace.subscribe((s, prev) => {
    if (activeId(s) !== activeId(prev) || s.tabs !== prev.tabs || s.notebooks !== prev.notebooks) refreshReferences();
  });
  return EditorView.domEventHandlers({
    mousedown(e, view) {
      if (!(e.metaKey || e.ctrlKey) || e.button !== 0) return false;
      const pos = view.posAtCoords({ x: e.clientX, y: e.clientY });
      const name = pos === null ? null : referenceAt(view.state, pos);
      const id = activeId(ws());
      if (!name || !id || !jumpTo(id, name)) return false;
      e.preventDefault();
      return true;
    },
  });
}
