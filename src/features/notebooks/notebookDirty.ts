import type { Text } from "@codemirror/state";
import { useSyncExternalStore } from "react";
import type { NotebookId, TabId } from "../../ipc/bindings";
import { cellMeta, metaKey } from "../../state/notebookFile";
import type { Notebook } from "../../state/notebooks";
import { useWorkspace, type Tab } from "../../state/workspace";
import { docOf, onBuffersChanged } from "../editor/editorStates";

const metaKeys = new WeakMap<Tab, string>();
const sameText = new WeakMap<Text, { text: string; same: boolean }>();

function keyOf(tab: Tab): string {
  let key = metaKeys.get(tab);
  if (key === undefined) {
    key = metaKey(cellMeta(tab));
    metaKeys.set(tab, key);
  }
  return key;
}

/** Memoized per doc, so a keystroke compares only the cell it changed. */
function holds(doc: Text | undefined, text: string): boolean {
  if (!doc) return text === "";
  const memo = sameText.get(doc);
  if (memo?.text === text) return memo.same;
  const same = doc.length === text.length && doc.toString() === text;
  sameText.set(doc, { text, same });
  return same;
}

/** Unsaved: a cell's text or what the file records about it changed, or a cell was added, closed, or moved. */
export function isNotebookDirty(notebook: Notebook, tabs: Record<TabId, Tab>, docs: (id: TabId) => Text | undefined = docOf): boolean {
  if (!notebook.file) return false;
  const saved = notebook.file.saved;
  if (!saved || saved.length !== notebook.cells.length) return true;
  return notebook.cells.some((id, i) => {
    const tab = tabs[id];
    const cell = saved[i];
    return !tab || !cell || keyOf(tab) !== cell.meta || !holds(docs(id), cell.text);
  });
}

export function notebookDirty(id: NotebookId): boolean {
  const { notebooks, tabs } = useWorkspace.getState();
  const notebook = notebooks[id];
  return notebook !== undefined && isNotebookDirty(notebook, tabs);
}

function subscribe(listener: () => void): () => void {
  const stopStore = useWorkspace.subscribe(listener);
  const stopBuffers = onBuffersChanged(listener);
  return () => {
    stopStore();
    stopBuffers();
  };
}

export function useNotebookDirty(id: NotebookId | undefined): boolean {
  const get = () => (id ? notebookDirty(id) : false);
  return useSyncExternalStore(subscribe, get, get);
}
