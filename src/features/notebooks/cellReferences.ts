import type { Extension } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { commands, type CellSource, type TabId } from "../../ipc/bindings";
import { databaseKindFor } from "../../state/databaseKind";
import { notebookOf } from "../../state/notebooks";
import { activeId, useWorkspace } from "../../state/workspace";
import { provideCellNames } from "../editor/completion";
import { docText } from "../editor/docText";
import { activateEditor, docOf, editorStateOf, referenceAt, showReferences } from "../editor/editorStates";

const ws = () => useWorkspace.getState();

function notebookTabs(id: TabId) {
  const s = ws();
  return (notebookOf(s.notebooks, id)?.cells ?? []).flatMap((c) => (s.tabs[c] ? [s.tabs[c]] : []));
}

/** Every cell of the tab's notebook, by title, for the engine to expand `{{name}}` from. */
export function notebookSources(id: TabId): CellSource[] {
  return notebookTabs(id).flatMap((t) => {
    const doc = docOf(t.id);
    return doc ? [{ name: t.title, text: docText(doc) }] : [];
  });
}

let timer: ReturnType<typeof setTimeout> | undefined;

/** Underlines the active cell's references, and marks the ones no cell is named after. */
export function refreshReferences(): void {
  clearTimeout(timer);
  timer = setTimeout(() => void markReferences(), 60);
}

async function markReferences(): Promise<void> {
  const id = activeId(ws());
  const tab = id ? ws().tabs[id] : undefined;
  if (!id || !tab) return;
  const doc = editorStateOf(id).doc;
  // A doc without "{{" holds no reference, so most edits skip the engine.
  if (!docText(doc).includes("{{")) return showReferences(id, doc, []);
  const found = await commands.cellReferences(docText(doc), databaseKindFor(tab.console.db, ws().configs));
  // A reply for an older doc or another cell is dropped: the change that made it stale already scheduled a newer pass.
  if (activeId(ws()) !== id || editorStateOf(id).doc !== doc) return;
  const names = new Set(notebookTabs(id).map((t) => t.title));
  showReferences(
    id,
    doc,
    found.map((r) => ({ from: r.range.from, to: r.range.to, name: r.name, known: names.has(r.name) })),
  );
}

function jumpTo(from: TabId, name: string): boolean {
  const target = notebookTabs(from).find((t) => t.title === name);
  if (!target || target.id === from) return false;
  ws().setActive(target.id);
  activateEditor(target.id);
  return true;
}

export function installCellReferences(): Extension {
  provideCellNames(() => {
    const id = activeId(ws());
    return id
      ? notebookTabs(id)
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
